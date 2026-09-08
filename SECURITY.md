# Security

Report a vulnerability privately through GitHub security advisories, on the
Security tab of `aaronsb/dwarf-eye`. Please do not open a public issue for
anything you think is exploitable. Expect a reply within a week or so; this is a
spare-time project.

Scope is the viewer itself: a local desktop application that speaks
RemoteFortressReader to a DFHack server on localhost, and reads and writes a
chunk cache under `~/.cache/dwarf-eye/`. It runs no server, accepts no inbound
connections and holds no credentials. Anything that lets untrusted data from the
DFHack connection or the cache corrupt memory, run code or escape the cache
directory is in scope. Vulnerabilities in Dwarf Fortress or DFHack belong to
those projects.
