//! Connection and call layer over [`crate::protocol`].

use crate::dfproto;
use crate::protocol as wire;
use anyhow::{Context, Result, bail};
use prost::Message;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 5000;

/// Identifies a remote method for binding.
#[derive(Clone, Copy, Debug)]
pub struct Method {
    pub plugin: Option<&'static str>,
    pub name: &'static str,
    pub input: &'static str,
    pub output: &'static str,
}

impl Method {
    /// A method on a plugin, e.g. `RemoteFortressReader`.
    pub const fn plugin(
        plugin: &'static str,
        name: &'static str,
        input: &'static str,
        output: &'static str,
    ) -> Self {
        Self { plugin: Some(plugin), name, input, output }
    }

    /// A method on the DFHack core.
    pub const fn core(name: &'static str, input: &'static str, output: &'static str) -> Self {
        Self { plugin: None, name, input, output }
    }

    fn key(&self) -> (&'static str, &'static str) {
        (self.plugin.unwrap_or(""), self.name)
    }
}

/// A connected DFHack remote client.
///
/// Method ids are negotiated lazily on first use and cached for the life of the
/// connection.
pub struct Client {
    reader: BufReader<TcpStream>,
    writer: BufWriter<TcpStream>,
    bound: HashMap<(&'static str, &'static str), i16>,
    /// Text notifications emitted by the most recent call.
    pub last_notices: Vec<String>,
}

impl Client {
    /// Connects to DFHack on localhost, honouring `DFHACK_PORT` when set.
    pub fn connect_local() -> Result<Self> {
        let port = std::env::var("DFHACK_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT);
        Self::connect(("127.0.0.1", port))
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> Result<Self> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .context("no socket address resolved")?;
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
            .with_context(|| format!("connecting to DFHack at {addr}"))?;
        stream.set_nodelay(true)?;

        let mut writer = BufWriter::new(stream.try_clone()?);
        let mut reader = BufReader::new(stream);

        // The handshake is a single 12-byte exchange, so drive it over one side
        // of the split and read the reply on the other.
        writer.write_all(&wire::handshake_request())?;
        writer.flush()?;
        wire::verify_handshake_reply(&mut reader)?;

        Ok(Self { reader, writer, bound: HashMap::new(), last_notices: Vec::new() })
    }

    /// Resolves a method to its numeric id, binding it if not already cached.
    pub fn bind(&mut self, method: Method) -> Result<i16> {
        if let Some(&id) = self.bound.get(&method.key()) {
            return Ok(id);
        }
        let request = dfproto::CoreBindRequest {
            method: method.name.to_string(),
            input_msg: method.input.to_string(),
            output_msg: method.output.to_string(),
            plugin: method.plugin.map(str::to_string),
        };
        let bytes = self
            .raw_call(wire::ID_BIND_METHOD, &request.encode_to_vec())
            .with_context(|| format!("binding {}", describe(&method)))?;
        let reply = dfproto::CoreBindReply::decode(&bytes[..])?;
        let id = i16::try_from(reply.assigned_id)
            .with_context(|| format!("server assigned an out-of-range id to {}", method.name))?;
        self.bound.insert(method.key(), id);
        Ok(id)
    }

    /// Binds if needed, then calls `method` with `request` and decodes the reply.
    pub fn call<I: Message, O: Message + Default>(&mut self, method: Method, request: &I) -> Result<O> {
        let id = self.bind(method)?;
        let bytes = self
            .raw_call(id, &request.encode_to_vec())
            .with_context(|| format!("calling {}", describe(&method)))?;
        O::decode(&bytes[..]).with_context(|| format!("decoding reply from {}", method.name))
    }

    /// Calls a method that takes no arguments.
    pub fn call_empty<O: Message + Default>(&mut self, method: Method) -> Result<O> {
        self.call(method, &dfproto::EmptyMessage {})
    }

    /// Sends one request and consumes replies until the result or failure arrives.
    fn raw_call(&mut self, id: i16, payload: &[u8]) -> Result<Vec<u8>> {
        self.last_notices.clear();
        wire::write_message(&mut self.writer, id, payload)?;

        loop {
            let (reply_id, size) = wire::read_header(&mut self.reader)?;
            match reply_id {
                wire::RPC_REPLY_RESULT => return wire::read_payload(&mut self.reader, size),
                wire::RPC_REPLY_FAIL => {
                    // Failure replies carry the error code in the size field and
                    // have no payload at all.
                    bail!("DFHack returned {} ({size})", wire::describe_error(size));
                }
                wire::RPC_REPLY_TEXT => {
                    let bytes = wire::read_payload(&mut self.reader, size)?;
                    let note = dfproto::CoreTextNotification::decode(&bytes[..])?;
                    for fragment in note.fragments {
                        self.last_notices.push(fragment.text);
                    }
                }
                other => bail!("unexpected reply id {other} from DFHack"),
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = wire::write_message(&mut self.writer, wire::RPC_REQUEST_QUIT, &[]);
    }
}

fn describe(method: &Method) -> String {
    match method.plugin {
        Some(p) => format!("{p}::{}", method.name),
        None => method.name.to_string(),
    }
}
