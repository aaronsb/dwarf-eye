//! Ties a DFHack connection to a [`World`], handling the one-time raw fetches.
//!
//! Dwarf Fortress loads a window of the world and DFHack reports blocks
//! relative to it. In adventure mode that window follows the character, so
//! the same ground comes back at new local coordinates after a walk. The
//! session pins a render origin at the window's position on connect and
//! converts every request and reply, so chunks keep their places and the map
//! paints in as the character travels.

use crate::cache::Cache;
use crate::palette::Palette;
use crate::world::{BLOCK, BlockBounds, TILES_PER_BLOCK, World};
use anyhow::Result;
use dfhack_remote::{Client, methods, rfr};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Tiles per unit of `MapInfo::block_pos`: DF positions the window in
/// 48-tile region tiles.
pub const REGION_TILE: i32 = 48;

/// How far under a column's floor blocks are still asked for. One level, so
/// the block that proved the floor has a neighbour beneath it to cull against.
const UNDER_FLOOR: i32 = 1;

/// How often the ground under the floors is asked about again.
///
/// A floor is the deepest thing anyone has seen from above, and it says nothing
/// about what has been dug or broken into under it since. An unforced
/// `GetBlockList` returns only blocks whose hash changed since the server last
/// sent them to this client, so re-asking under the floors costs round trips
/// and a hash check per block, not payload. Half a minute keeps a cavern that
/// opens under the character on screen inside the time it takes to walk to it,
/// while adding two requests a minute to a pass loop that already runs one to
/// three times a second.
const PROBE_EVERY: Duration = Duration::from_secs(30);

/// What one collection pass cost, and what it would have cost asking for the
/// whole box the way a fixed depth does.
#[derive(Default, Clone)]
pub struct Pass {
    pub asked: i32,
    pub whole_box: i32,
    pub arrived: usize,
    /// Column floors still standing when the pass began.
    pub floors: usize,
    /// Blocks asked for under the floors, and the round trips that took. Zero
    /// on a pass that neither probed nor followed a neighbour down.
    pub probed: i32,
    pub probe_requests: i32,
    /// Blocks the probe kept: ground revealed under a floor, by absolute key.
    pub probe_kept: Vec<(i32, i32, i32)>,
    /// Columns whose floor was dropped this pass, with the level of the ground
    /// that dropped it. Absolute block columns.
    pub reopened: Vec<((i32, i32), i32)>,
    /// Chunks a forced pass asked about and the game did not answer with, in
    /// render keys. They have been dropped from the world and the cache.
    pub stale: Vec<(i32, i32, i32)>,
    /// The window moved while the pass was in flight, so it neither trusted its
    /// own frame nor dropped anything.
    pub window_moved: bool,
    /// Blocks asked for in the forced strip the window had just uncovered, and
    /// how many boxes that strip took.
    pub leading: i32,
    pub leading_boxes: usize,
    /// How many bands the travel heading cut the window into: one when nothing
    /// is travelling, otherwise ahead, the sides, and behind.
    pub bands: usize,
}

/// Which levels of a column a descent asks about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// The top of the window down to one level under each column's floor: the
    /// ground anyone has seen, and the blind block that proves where it stops.
    Surface,
    /// Everything under the floors. Nothing is kept there unless it has been
    /// revealed, so the pass costs round trips rather than rock.
    Probe,
}

/// A column where ground was seen deeper than it had been.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Sighting {
    column: (i32, i32),
    /// The deepest level newly seen into.
    z: i32,
    /// The column had a floor at or over `z`, and it has been dropped.
    opened: bool,
}

/// Where each block column stops being worth asking about, and how deep anyone
/// has seen into it.
///
/// Dwarf Fortress hands out the rock under a map whether or not anyone has seen
/// it, so a column is asked about top down and stops at its floor: the first
/// block whose whole 256 tiles come back hidden. The rest of this is about
/// getting a column back once the ground under that floor is opened — by a
/// tunnel from a neighbour, by a cavern an event breaks into, by a pit mine dug
/// out of sight — none of which the floor block itself ever reports, because it
/// stays exactly as hidden as it was.
#[derive(Default)]
struct Floors {
    /// Where each column stops: the shallowest level whose whole block came
    /// back hidden with nothing revealed under it.
    stops: HashMap<(i32, i32), i32>,
    /// The deepest level seen into, per column. A blind block over it is an
    /// overhang or a stale answer, never a floor, so this keeps a column from
    /// closing over ground that is already on screen.
    seen: HashMap<(i32, i32), i32>,
}

impl Floors {
    /// Starts from what an earlier session wrote. Nothing is known about what
    /// has been seen until blocks arrive.
    fn new(stops: HashMap<(i32, i32), i32>) -> Self {
        Self { stops, seen: HashMap::new() }
    }

    fn stops(&self) -> &HashMap<(i32, i32), i32> {
        &self.stops
    }

    fn len(&self) -> usize {
        self.stops.len()
    }

    fn stop(&self, column: (i32, i32)) -> Option<i32> {
        self.stops.get(&column).copied()
    }

    /// Whether a descent asks about a column at a level. `chasing` holds the
    /// columns this descent has already reopened: they have no floor yet, and
    /// the probe that dropped it is the one that has to find the new one.
    fn open(
        &self,
        column: (i32, i32),
        z: i32,
        phase: Phase,
        chasing: &HashSet<(i32, i32)>,
    ) -> bool {
        match self.stop(column) {
            None => phase == Phase::Surface || chasing.contains(&column),
            Some(floor) => match phase {
                Phase::Surface => z >= floor - UNDER_FLOOR,
                Phase::Probe => z < floor - UNDER_FLOOR,
            },
        }
    }

    /// Folds one reply into the floors: `(column, level, blind)` per block that
    /// came back, blind meaning nobody has seen into any of its 256 tiles.
    ///
    /// Returns the columns where ground was seen deeper than it had been. The
    /// first sighting of a column is not one of them: it is the descent finding
    /// the ground, not the ground changing.
    fn settle(&mut self, batch: &[((i32, i32), i32, bool)]) -> Vec<Sighting> {
        // Deepest revealed level and every blind level, per column.
        let mut columns: HashMap<(i32, i32), (Option<i32>, Vec<i32>)> = HashMap::new();
        for &(column, z, blind) in batch {
            let entry = columns.entry(column).or_insert((None, Vec::new()));
            if blind {
                entry.1.push(z);
            } else {
                entry.0 = Some(entry.0.map_or(z, |v: i32| v.min(z)));
            }
        }

        let mut news = Vec::new();
        for (column, (revealed, blind)) in columns {
            let known = self.seen.get(&column).copied();
            if let Some(z) = revealed {
                let deeper = known.is_none_or(|k| z < k);
                if deeper {
                    self.seen.insert(column, z);
                }
                // Ground at or under the floor: whatever the floor block still
                // says, the belief that nothing under it had been seen is now
                // simply wrong.
                let opened = self.stops.get(&column).is_some_and(|&floor| z <= floor);
                if opened {
                    self.stops.remove(&column);
                }
                if opened || (deeper && known.is_some()) {
                    news.push(Sighting { column, z, opened });
                }
            }
            // The block that was the deepest thing seen coming back wholly
            // hidden supersedes the memory of having seen into it. A blind
            // block anywhere over that level is only an overhang and leaves
            // the memory alone.
            if self
                .seen
                .get(&column)
                .is_some_and(|deepest| blind.contains(deepest))
            {
                self.seen.remove(&column);
            }
            // A floor stands until ground is seen under it. Only a column
            // without one is looking, so a blind block arriving over a standing
            // floor can never raise it back up, and one arriving under it can
            // never push it further down than the descent has actually got.
            if !self.stops.contains_key(&column) {
                let bar = self.seen.get(&column).copied().unwrap_or(i32::MAX);
                if let Some(&z) = blind.iter().filter(|&&z| z < bar).max() {
                    self.stops.insert(column, z);
                }
            }
        }
        news
    }

    /// Columns to look into now because a neighbour has ground under their
    /// floor, with the level to reach. A tunnel arrives in one column and its
    /// continuation is in the next, and the neighbour's own pass stops too high
    /// to see it.
    ///
    /// Only the four sides: a corner is reached through one of them on the
    /// pass after, which is soon enough and keeps the box small.
    fn spread(&self, news: &[Sighting]) -> Vec<((i32, i32), i32)> {
        let mut out: HashMap<(i32, i32), i32> = HashMap::new();
        for sighting in news {
            for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                let side = (sighting.column.0 + dx, sighting.column.1 + dy);
                // Only where the neighbour's own descent would not go anyway.
                if self.stop(side).is_some_and(|floor| sighting.z < floor - UNDER_FLOOR) {
                    let reach = out.entry(side).or_insert(sighting.z);
                    *reach = (*reach).min(sighting.z);
                }
            }
        }
        out.into_iter().collect()
    }

    /// Forgets the floor of the column the character has gone down into, and of
    /// the ring around them.
    ///
    /// A floor high on a hillside is over their head and means nothing, and
    /// standing on the ground already puts them a level or two over the rock
    /// nobody has seen into. What counts is going down into the unseen part of
    /// the column they are in.
    fn forget_under(&mut self, here: (i32, i32, i32)) {
        self.stops.retain(|&(bx, by), &mut floor| {
            (bx - here.0).abs() > 1 || (by - here.1).abs() > 1 || here.2 > floor
        });
    }
}

/// What one descent through the window brought back.
#[derive(Default)]
struct Descent {
    arrived: Vec<(i32, i32, i32)>,
    asked: i32,
    requests: i32,
    news: Vec<Sighting>,
    /// Every chunk the descent asked about, in render keys. Only a forced
    /// descent fills this: what an unforced one leaves out means unchanged.
    asked_keys: Vec<(i32, i32, i32)>,
    /// A reply came back in a different window frame from the one the descent
    /// began in, so its box says nothing about what is or is not there.
    moved: bool,
}

/// The keys a forced pass asked about that nothing came back for.
///
/// The live window is the game's own answer about the land it covers, so a
/// block it does not hand over on a forced request is not there: DFHack drops a
/// block whose 256 tiles are all air or nothing. Anything the cache still shows
/// at that key is land the game has moved on from, and it stays on screen for
/// as long as it is never contradicted, because absence is what a hash-gated
/// reply is made of.
fn unanswered(
    asked: &[(i32, i32, i32)],
    arrived: &[(i32, i32, i32)],
) -> Vec<(i32, i32, i32)> {
    let came: HashSet<(i32, i32, i32)> = arrived.iter().copied().collect();
    let mut out: Vec<_> = asked.iter().copied().filter(|k| !came.contains(k)).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// A box cut into the bands a travelling character wants in order: the blocks
/// ahead of them, the strip they stand in, then the blocks behind. `here` is
/// their block column in the same coordinates as the box.
///
/// A request is a box, so the cut is a single plane across the axis the heading
/// mostly runs on. A diagonal therefore puts one flanking quarter in with the
/// blocks ahead, which is the price of not turning one request into four; what
/// matters is that the land the character is walking into is asked for first.
/// Nothing here widens the box: every band is a slice of it.
fn travel_bands(
    local: &BlockBounds,
    here: (i32, i32),
    heading: Option<(f32, f32)>,
) -> Vec<BlockBounds> {
    let Some((hx, hy)) = heading else { return vec![*local] };
    let across_x = hx.abs() >= hy.abs();
    let (lo, hi, at, forward) = if across_x {
        (local.min_x, local.max_x, here.0, hx >= 0.0)
    } else {
        (local.min_y, local.max_y, here.1, hy >= 0.0)
    };
    let at = at.clamp(lo, hi - 1);
    let spans = if forward {
        [(at + 1, hi), (at, at + 1), (lo, at)]
    } else {
        [(lo, at), (at, at + 1), (at + 1, hi)]
    };
    spans
        .into_iter()
        .filter(|(a, b)| a < b)
        .map(|(a, b)| {
            if across_x {
                BlockBounds { min_x: a, max_x: b, ..*local }
            } else {
                BlockBounds { min_y: a, max_y: b, ..*local }
            }
        })
        .collect()
}

/// Where a reply's own local coordinates sit, in render blocks and levels.
///
/// Every `BlockList` carries the window's position at the moment the server
/// built it. The window follows the character, so a pass that spans a
/// re-centring gets its replies in two frames a region tile apart; placing the
/// later ones by the frame the pass began in wrote land 48 tiles from where it
/// belonged, and the cache kept it there. `None` when the reply does not say,
/// which leaves the pass's own frame standing.
fn reply_frame(
    list: &rfr::BlockList,
    origin: (i32, i32, i32),
    z: i32,
) -> Option<(i32, i32, i32)> {
    let (x, y) = (list.map_x?, list.map_y?);
    Some((
        (x * REGION_TILE - origin.0).div_euclid(BLOCK),
        (y * REGION_TILE - origin.1).div_euclid(BLOCK),
        z,
    ))
}

pub struct Session {
    pub client: Client,
    pub world: World,
    pub map_info: rfr::MapInfo,
    pub version: rfr::VersionInfo,
    /// Absolute position of the render origin: tiles in x/y, z-level in z.
    origin: (i32, i32, i32),
    /// Chunks from earlier sessions, and where new ones are written.
    cache: Option<Cache>,
    /// Where each block column turns to unrevealed rock, and how deep anyone
    /// has seen into it.
    ///
    /// Dwarf Fortress hands out the rock under a map whether or not anyone has
    /// seen it, and a window is 81 columns wide, so asking for a fixed depth
    /// under the character fetched, cached and meshed thousands of blocks of
    /// solid dark. A column is asked about top down and stops at its floor
    /// instead.
    floors: Floors,
    /// Where the character is, in render tiles and level, so a column they
    /// descend into is probed afresh.
    viewer: (i32, i32, i32),
    /// Which way they are travelling on the ground plan, as a unit vector east
    /// and south, or `None` when nothing is travelling. It orders the slabs; it
    /// never changes which blocks a pass asks about.
    heading: Option<(f32, f32)>,
    /// When the ground under the floors was last asked about.
    probed: Instant,
    /// What the last pass cost.
    pub last_pass: Pass,
}

/// Absolute position of a window's corner: tiles in x/y, z-level in z.
fn window_origin(info: &rfr::MapInfo) -> (i32, i32, i32) {
    (info.block_pos_x() * REGION_TILE, info.block_pos_y() * REGION_TILE, info.block_pos_z())
}

impl Session {
    /// Connects and pulls the raws that stay fixed for the life of the world.
    pub fn connect_local() -> Result<Self> {
        let mut client = Client::connect_local()?;
        let version = client.call_empty(methods::GET_VERSION_INFO)?;
        let map_info: rfr::MapInfo = client.call_empty(methods::GET_MAP_INFO)?;
        let tiletypes = client.call_empty(methods::GET_TILETYPE_LIST)?;
        let materials = client.call_empty(methods::GET_MATERIAL_LIST)?;
        let world = World::new(Palette::new(tiletypes, materials));
        let origin = window_origin(&map_info);
        let cache = Cache::open(map_info.world_name_english(), map_info.save_name()).ok();
        let floors = cache.as_ref().map(Cache::load_floors).unwrap_or_default();
        Ok(Self {
            client,
            world,
            map_info,
            version,
            origin,
            cache,
            floors: Floors::new(floors),
            viewer: (0, 0, 0),
            heading: None,
            // The first probe comes a period in, so the first pass — which is
            // forced, and asks for everything the floors allow — is untouched.
            probed: Instant::now(),
            last_pass: Pass::default(),
        })
    }

    /// Tells the session where the character is, in render tiles and level, so
    /// a column they have gone down into is asked about again.
    pub fn watch_from(&mut self, at: (i32, i32, i32)) {
        self.viewer = at;
    }

    /// Tells the session which way the character is travelling, as a unit
    /// vector east and south on the ground plan, so the slabs ahead of them are
    /// asked for first. `None` puts the window back in one piece.
    pub fn travel_toward(&mut self, heading: Option<(f32, f32)>) {
        self.heading = heading;
    }

    /// How many columns have a known floor.
    pub fn floor_count(&self) -> usize {
        self.floors.len()
    }

    /// The shallowest and deepest known floor, in levels under the character.
    pub fn floor_depths(&self) -> (i32, i32) {
        let here = self.viewer.2 + self.origin.2;
        self.floors.stops().values().fold((0, 0), |(lo, hi), &floor| {
            ((lo).min(floor - here), (hi).max(floor - here))
        })
    }

    /// Absolute key of a render-space chunk key.
    fn absolute(&self, key: (i32, i32, i32)) -> (i32, i32, i32) {
        (key.0 + self.origin.0 / BLOCK, key.1 + self.origin.1 / BLOCK, key.2 + self.origin.2)
    }

    fn relative(&self, key: (i32, i32, i32)) -> (i32, i32, i32) {
        (key.0 - self.origin.0 / BLOCK, key.1 - self.origin.1 / BLOCK, key.2 - self.origin.2)
    }

    pub fn cache_dir(&self) -> Option<&std::path::Path> {
        self.cache.as_ref().map(Cache::dir)
    }

    /// Brings every cached chunk of this world into the render space. Returns
    /// their keys.
    ///
    /// A column whose lowest cached chunk is sparse holds canopy with no
    /// ground under it, from a fetch that did not reach down far enough. Those
    /// are left out and their files removed, so they are fetched afresh when
    /// the window returns.
    ///
    /// Chunks below a column's floor are dropped as well. They are unrevealed
    /// rock, cached by an earlier build that asked for a fixed depth, and they
    /// cost the whole of a restore: fetched, stored, restored and meshed for
    /// something nobody has seen.
    pub fn restore_cache(&mut self) -> Vec<(i32, i32, i32)> {
        let Some(cache) = &self.cache else { return Vec::new() };
        let Ok(mut entries) = cache.load_all() else { return Vec::new() };
        entries.sort_by_key(|(key, _)| *key);

        let mut lowest: std::collections::HashMap<(i32, i32), (i32, bool)> = std::collections::HashMap::new();
        for (key, voxels) in &entries {
            let filled = voxels.iter().filter(|v| !v.solid.is_empty()).count();
            let grounded = filled * 2 >= voxels.len();
            let entry = lowest.entry((key.0, key.1)).or_insert((key.2, grounded));
            if key.2 < entry.0 {
                *entry = (key.2, grounded);
            }
        }

        let mut keys = Vec::new();
        for (absolute, voxels) in entries {
            if !lowest.get(&(absolute.0, absolute.1)).is_some_and(|(_, grounded)| *grounded) {
                cache.remove(absolute);
                continue;
            }
            // Wholly unrevealed rock, cached by a build that asked for a fixed
            // depth under the character. The test is the chunk's own tiles
            // rather than the floor file, so nothing anyone has actually seen
            // can be thrown away by a stale sidecar.
            if voxels.len() == TILES_PER_BLOCK && voxels.iter().all(|v| v.hidden) {
                cache.remove(absolute);
                continue;
            }
            let key = self.relative(absolute);
            if self.world.restore(key, voxels) {
                keys.push(key);
            }
        }
        keys
    }

    /// Writes chunks to the cache under their absolute keys.
    pub fn persist(&self, keys: &[(i32, i32, i32)]) {
        let Some(cache) = &self.cache else { return };
        for &key in keys {
            let Some(chunk) = self.world.chunk(key.0, key.1, key.2) else { continue };
            // A block nobody has seen into is not land worth keeping: it is
            // the same rock every world has under it, and it is what let the
            // cache grow to thirty times the size of the map anyone has walked.
            if chunk.voxels.len() == TILES_PER_BLOCK && chunk.voxels.iter().all(|v| v.hidden) {
                continue;
            }
            let _ = cache.store(self.absolute(key), chunk);
        }
        cache.store_floors(self.floors.stops());
    }

    /// How many chunk files the cache holds, and what they weigh.
    pub fn cache_size(&self) -> (usize, u64) {
        self.cache.as_ref().map(Cache::size).unwrap_or((0, 0))
    }

    /// Re-reads where the window sits. True when it has moved since the last
    /// read.
    pub fn refresh_window(&mut self) -> Result<bool> {
        let info: rfr::MapInfo = self.client.call_empty(methods::GET_MAP_INFO)?;
        let moved = window_origin(&info) != window_origin(&self.map_info);
        self.map_info = info;
        Ok(moved)
    }

    /// The render origin, in absolute tiles and z-level.
    pub fn origin(&self) -> (i32, i32, i32) {
        self.origin
    }

    /// What to add to window-local coordinates to reach render coordinates:
    /// tiles in x/y, levels in z.
    pub fn shift(&self) -> (i32, i32, i32) {
        let w = window_origin(&self.map_info);
        (w.0 - self.origin.0, w.1 - self.origin.1, w.2 - self.origin.2)
    }

    /// Where the player is looking, in render coordinates.
    pub fn view_center(&mut self) -> Result<(i32, i32, i32)> {
        let view: rfr::ViewInfo = self.client.call_empty(methods::GET_VIEW_INFO)?;
        let (sx, sy, sz) = self.shift();
        Ok((
            view.view_pos_x() + view.view_size_x() / 2 + sx,
            view.view_pos_y() + view.view_size_y() / 2 + sy,
            view.view_pos_z() + sz,
        ))
    }

    /// The whole live window in render-space blocks, between two render
    /// levels. Everything Dwarf Fortress currently holds sits inside it.
    pub fn window_bounds(&self, z_lo: i32, z_hi: i32) -> BlockBounds {
        let (sx, sy, sz) = self.shift();
        let (bx, by) = (sx.div_euclid(BLOCK), sy.div_euclid(BLOCK));
        BlockBounds {
            min_x: bx,
            max_x: bx + self.map_info.block_size_x(),
            min_y: by,
            max_y: by + self.map_info.block_size_y(),
            min_z: z_lo.max(sz),
            max_z: z_hi.min(sz + self.map_info.block_size_z()),
        }
    }

    /// A render-space box clipped to the window, in the window's own block
    /// coordinates. `None` when none of it is inside.
    fn localize(&self, bounds: BlockBounds, shift: (i32, i32, i32)) -> Option<BlockBounds> {
        let local = BlockBounds {
            min_x: (bounds.min_x - shift.0).max(0),
            max_x: (bounds.max_x - shift.0).min(self.map_info.block_size_x()),
            min_y: (bounds.min_y - shift.1).max(0),
            max_y: (bounds.max_y - shift.1).min(self.map_info.block_size_y()),
            min_z: (bounds.min_z - shift.2).max(0),
            max_z: (bounds.max_z - shift.2).min(self.map_info.block_size_z()),
        };
        (local.min_x < local.max_x && local.min_y < local.max_y && local.min_z < local.max_z)
            .then_some(local)
    }

    /// Fetches the blocks of `bounds` (render-space blocks and levels) that
    /// fall inside the window, and folds them into the world. Returns the keys
    /// of the chunks that arrived.
    ///
    /// With `force` unset the server sends only what has changed since it last
    /// sent it: DFHack keeps one hash per block of the tiletypes and another of
    /// the designations, and the two decide separately whether a block arrives
    /// with tiles, with the seen bits, with both, or not at all. The hashes are
    /// the plugin's, not this connection's, so asking for ground nobody has
    /// changed costs round trips rather than payload, and a second viewer on
    /// the same game reads changes the first one would have been sent.
    ///
    /// Slabs descend rather than rise, and a column drops out of the request
    /// as soon as a block of it comes back wholly hidden: that is the floor of
    /// what anyone has seen, and everything under it is rock the game would
    /// hand over and the renderer would then have to carry.
    ///
    /// A column comes back when the character goes down into it, when the block
    /// that proved its floor is revealed, when the periodic probe finds ground
    /// under the floor, or when a neighbour does and this column is next to it.
    ///
    /// `leading` is the part of the box the last pass did not cover, in render
    /// blocks: the strip a window shift or a change of depth has just uncovered.
    /// It is asked for first and forced, because an unforced request would weigh
    /// it against a hash from land that is no longer there.
    pub fn fetch(&mut self, bounds: BlockBounds, force: bool) -> Result<Vec<(i32, i32, i32)>> {
        self.fetch_leading(bounds, force, &[])
    }

    /// [`Session::fetch`] with a leading strip in front of it.
    pub fn fetch_leading(
        &mut self,
        bounds: BlockBounds,
        force: bool,
        leading: &[BlockBounds],
    ) -> Result<Vec<(i32, i32, i32)>> {
        let (sx, sy, sz) = self.shift();
        let shift_blocks = (sx.div_euclid(BLOCK), sy.div_euclid(BLOCK), sz);
        let Some(local) = self.localize(bounds, shift_blocks) else {
            return Ok(Vec::new());
        };
        // A column the character has walked down into is one whose floor was
        // only ever the limit of what they had seen from above.
        //
        // A forced pass does not throw the floors away, or every step the
        // character takes would pay for the descent again. It re-reads them
        // instead: the block that proved a floor comes back with everything
        // else, and if it has been revealed since, the column reopens.
        let here = (
            self.viewer.0.div_euclid(BLOCK) + self.origin.0 / BLOCK,
            self.viewer.1.div_euclid(BLOCK) + self.origin.1 / BLOCK,
            self.viewer.2 + self.origin.2,
        );
        self.floors.forget_under(here);
        let kept = self.floors.len();

        let mut pass = Pass {
            whole_box: (local.max_x - local.min_x)
                * (local.max_y - local.min_y)
                * (local.max_z - local.min_z),
            floors: kept,
            ..Default::default()
        };
        let mut arrived = Vec::new();
        let mut news = Vec::new();
        let mut asked_keys = Vec::new();
        let mut moved = false;

        // The strip first, forced, in the order the caller put it in — the
        // leading edge before the trailing one.
        for &strip in leading {
            let Some(strip) = self.localize(strip, shift_blocks) else { continue };
            let out = self.descend(&strip, shift_blocks, true, Phase::Surface)?;
            pass.asked += out.asked;
            pass.leading += out.asked;
            pass.leading_boxes += 1;
            moved |= out.moved;
            asked_keys.extend(out.asked_keys);
            arrived.extend(out.arrived);
            news.extend(out.news);
        }

        // Then the window itself, cut into the bands the heading orders: the
        // blocks ahead of the character, the strip they stand in, then the ones
        // behind. Each band is a slice of the same box, so nothing is asked for
        // that the one descent would not have asked for.
        let local_here = (
            self.viewer.0.div_euclid(BLOCK) - shift_blocks.0,
            self.viewer.1.div_euclid(BLOCK) - shift_blocks.1,
        );
        let bands = travel_bands(&local, local_here, self.heading);
        pass.bands = bands.len();
        for band in &bands {
            let out = self.descend(band, shift_blocks, force, Phase::Surface)?;
            pass.asked += out.asked;
            moved |= out.moved;
            asked_keys.extend(out.asked_keys);
            arrived.extend(out.arrived);
            news.extend(out.news);
        }

        // While the live window covers a block, the game is the authority on
        // it. A forced request is the whole truth about the box it asked for,
        // so a chunk still standing where nothing came back is land the game
        // has moved on from: a felled tree, a block that is now air, or a chunk
        // written 48 tiles off by a pass that spanned a re-centring. Only a
        // forced descent fills `asked_keys`, so an unforced sweep with a forced
        // strip in front of it drops only inside that strip.
        pass.window_moved = moved;
        if !moved {
            for key in unanswered(&asked_keys, &arrived) {
                if self.world.remove(key) {
                    pass.stale.push(key);
                }
                if let Some(cache) = &self.cache {
                    cache.remove(self.absolute(key));
                }
            }
        }

        // Under the floors, now and then. A forced pass is left alone: it is
        // the character stepping into new ground or the first pass of a
        // session, and both are expensive enough already.
        if !force && self.probed.elapsed() >= PROBE_EVERY {
            self.probed = Instant::now();
            let deep = self.descend(&local, shift_blocks, false, Phase::Probe)?;
            pass.probed += deep.asked;
            pass.probe_requests += deep.requests;
            pass.probe_kept.extend(deep.arrived.iter().map(|&k| self.absolute(k)));
            arrived.extend(deep.arrived);
            news.extend(deep.news);
        }

        // Ground seen under a neighbour's floor is a reason to look there now
        // rather than at the end of the next probe period: that is what a
        // tunnel crossing a block boundary looks like from here.
        if !force {
            let targets = self.floors.spread(&news);
            if let Some(side) = self.side_box(&local, &targets, shift_blocks) {
                let sideways = self.descend(&side, shift_blocks, false, Phase::Probe)?;
                pass.probed += sideways.asked;
                pass.probe_requests += sideways.requests;
                pass.probe_kept.extend(sideways.arrived.iter().map(|&k| self.absolute(k)));
                arrived.extend(sideways.arrived);
                news.extend(sideways.news);
            }
        }

        // The bands sweep over the leading strip as well, so a block can arrive
        // twice in one pass. Meshing it twice is only waste.
        arrived.sort_unstable();
        arrived.dedup();
        pass.arrived = arrived.len();
        pass.reopened =
            news.iter().filter(|s| s.opened).map(|s| (s.column, s.z)).collect();
        self.last_pass = pass;
        Ok(arrived)
    }

    /// One descent through the window: slabs from the top down, each request
    /// only as wide as the columns still open at that level.
    ///
    /// DFHack refuses any reply over 64 MiB with a link failure, and a busy
    /// block can run tens of kilobytes, so requests go down in slabs.
    fn descend(
        &mut self,
        local: &BlockBounds,
        shift: (i32, i32, i32),
        force: bool,
        phase: Phase,
    ) -> Result<Descent> {
        const MAX_BLOCKS_PER_REQUEST: i32 = 500;
        let mut out = Descent::default();
        let mut chasing: HashSet<(i32, i32)> = HashSet::new();
        let mut top = local.max_z;
        while top > local.min_z {
            // Only the columns that still have something to show, and only as
            // wide a box as they need. A request is a box, so a column deeper
            // than its neighbours drags them down with it; the box narrowing
            // as the slabs descend is what keeps that cheap.
            let Some(wide) = self.open_box(local, top - 1, shift, phase, &chasing) else {
                // The surface descent is done once every column has bottomed
                // out. A probe's columns open as it passes under their floors,
                // so it steps down instead of stopping.
                if phase == Phase::Surface {
                    break;
                }
                top -= 1;
                continue;
            };
            let footprint = (wide.max_x - wide.min_x) * (wide.max_y - wide.min_y);
            let levels = (MAX_BLOCKS_PER_REQUEST / footprint.max(1)).max(1);
            let bottom = (top - levels).max(local.min_z);
            let request = rfr::BlockRequest {
                blocks_needed: Some(footprint * (top - bottom)),
                min_x: Some(wide.min_x),
                max_x: Some(wide.max_x),
                min_y: Some(wide.min_y),
                max_y: Some(wide.max_y),
                min_z: Some(bottom),
                max_z: Some(top),
                force_reload: Some(force),
            };
            out.asked += footprint * (top - bottom);
            out.requests += 1;
            let mut list: rfr::BlockList = self.client.call(methods::GET_BLOCK_LIST, &request)?;
            // The reply says which window it was built in. A pass that spans a
            // re-centring places its later blocks by that frame rather than by
            // the one it started in.
            let frame = reply_frame(&list, self.origin, shift.2).unwrap_or(shift);
            if frame != shift {
                out.moved = true;
            } else if force {
                for z in bottom..top {
                    for bx in wide.min_x..wide.max_x {
                        for by in wide.min_y..wide.max_y {
                            out.asked_keys.push((bx + shift.0, by + shift.1, z + shift.2));
                        }
                    }
                }
            }
            let shift = frame;
            // Read the levels off the reply rather than off the world, so a
            // probe can weigh a block it is not going to keep.
            let batch = self.read_levels(&list, shift);
            let mut wanted = Vec::new();
            if phase == Phase::Probe {
                // Under the floors nothing is kept unless it has been revealed.
                // The rock down there is exactly what the floors were drawn to
                // stop carrying, so it is weighed and dropped.
                //
                // Ground whose seen bits have moved but whose tiletypes have
                // not — the fog of war lifting off a cavern that was already
                // cut — arrives with nothing to draw. Those blocks are asked
                // for again, forced, or they would never come back at all.
                for block in &list.map_blocks {
                    if seen_into(block) == Some(false) && block.tiles.is_empty() {
                        wanted.push((
                            block.map_x.div_euclid(BLOCK),
                            block.map_y.div_euclid(BLOCK),
                            block.map_z,
                        ));
                    }
                }
                list.map_blocks.retain(|block| seen_into(block) == Some(false));
            }
            out.arrived.extend(self.world.absorb(list, shift));
            if !wanted.is_empty() {
                let again = self.refetch(&wanted, shift)?;
                out.asked += again.asked;
                out.requests += again.requests;
                out.arrived.extend(again.arrived);
            }
            for sighting in self.floors.settle(&batch) {
                if sighting.opened {
                    chasing.insert(sighting.column);
                }
                out.news.push(sighting);
            }
            top = bottom;
        }
        Ok(out)
    }

    /// Asks again, forced, for blocks that came back revealed with nothing to
    /// draw. One request per level, over the box the finds at that level
    /// occupy: they are rare, so the box is small.
    fn refetch(&mut self, blocks: &[(i32, i32, i32)], shift: (i32, i32, i32)) -> Result<Descent> {
        let mut out = Descent::default();
        let mut levels: HashMap<i32, BlockBounds> = HashMap::new();
        for &(bx, by, z) in blocks {
            levels
                .entry(z)
                .and_modify(|b| {
                    b.min_x = b.min_x.min(bx);
                    b.max_x = b.max_x.max(bx + 1);
                    b.min_y = b.min_y.min(by);
                    b.max_y = b.max_y.max(by + 1);
                })
                .or_insert(BlockBounds {
                    min_x: bx,
                    max_x: bx + 1,
                    min_y: by,
                    max_y: by + 1,
                    min_z: z,
                    max_z: z + 1,
                });
        }
        for (z, box_) in levels {
            let footprint = (box_.max_x - box_.min_x) * (box_.max_y - box_.min_y);
            let request = rfr::BlockRequest {
                blocks_needed: Some(footprint),
                min_x: Some(box_.min_x),
                max_x: Some(box_.max_x),
                min_y: Some(box_.min_y),
                max_y: Some(box_.max_y),
                min_z: Some(z),
                max_z: Some(z + 1),
                force_reload: Some(true),
            };
            out.asked += footprint;
            out.requests += 1;
            let mut list: rfr::BlockList = self.client.call(methods::GET_BLOCK_LIST, &request)?;
            list.map_blocks.retain(|block| seen_into(block) == Some(false));
            out.arrived.extend(self.world.absorb(list, shift));
        }
        Ok(out)
    }

    /// The box of columns worth asking about at a level, in local block
    /// coordinates. `None` when none of them is open there.
    fn open_box(
        &self,
        local: &BlockBounds,
        z: i32,
        shift: (i32, i32, i32),
        phase: Phase,
        chasing: &HashSet<(i32, i32)>,
    ) -> Option<BlockBounds> {
        let mut open: Option<BlockBounds> = None;
        for bx in local.min_x..local.max_x {
            for by in local.min_y..local.max_y {
                let column = (bx + shift.0 + self.origin.0 / BLOCK, by + shift.1 + self.origin.1 / BLOCK);
                if !self.floors.open(column, z + shift.2 + self.origin.2, phase, chasing) {
                    continue;
                }
                open = Some(match open {
                    None => BlockBounds { min_x: bx, max_x: bx + 1, min_y: by, max_y: by + 1, min_z: z, max_z: z + 1 },
                    Some(b) => BlockBounds {
                        min_x: b.min_x.min(bx),
                        max_x: b.max_x.max(bx + 1),
                        min_y: b.min_y.min(by),
                        max_y: b.max_y.max(by + 1),
                        ..b
                    },
                });
            }
        }
        open
    }

    /// The window narrowed to the columns a neighbour's find is worth chasing
    /// into, reaching down to the level of the deepest of those finds.
    fn side_box(
        &self,
        local: &BlockBounds,
        targets: &[((i32, i32), i32)],
        shift: (i32, i32, i32),
    ) -> Option<BlockBounds> {
        let (ox, oy, oz) = (self.origin.0 / BLOCK, self.origin.1 / BLOCK, self.origin.2);
        let mut side: Option<BlockBounds> = None;
        for &((cx, cy), z) in targets {
            let (bx, by) = (cx - shift.0 - ox, cy - shift.1 - oy);
            if bx < local.min_x || bx >= local.max_x || by < local.min_y || by >= local.max_y {
                continue;
            }
            let bottom = (z - shift.2 - oz).max(local.min_z);
            side = Some(match side {
                None => BlockBounds {
                    min_x: bx,
                    max_x: bx + 1,
                    min_y: by,
                    max_y: by + 1,
                    min_z: bottom,
                    max_z: local.max_z,
                },
                Some(b) => BlockBounds {
                    min_x: b.min_x.min(bx),
                    max_x: b.max_x.max(bx + 1),
                    min_y: b.min_y.min(by),
                    max_y: b.max_y.max(by + 1),
                    min_z: b.min_z.min(bottom),
                    max_z: b.max_z,
                },
            });
        }
        side
    }

    /// What one reply says about each column: the level of every block that
    /// came back, and whether anyone has seen into it.
    fn read_levels(
        &self,
        list: &rfr::BlockList,
        shift: (i32, i32, i32),
    ) -> Vec<((i32, i32), i32, bool)> {
        let (ox, oy, oz) = (self.origin.0 / BLOCK, self.origin.1 / BLOCK, self.origin.2);
        list.map_blocks
            .iter()
            .filter_map(|block| {
                let blind = seen_into(block)?;
                let column = (
                    block.map_x.div_euclid(BLOCK) + shift.0 + ox,
                    block.map_y.div_euclid(BLOCK) + shift.1 + oy,
                );
                Some((column, block.map_z + shift.2 + oz, blind))
            })
            .collect()
    }
}

/// What a reply says about who has seen into a block: `Some(true)` when all 256
/// tiles are hidden, `Some(false)` when any of them is not, `None` when the
/// reply says nothing about it.
///
/// DFHack weighs a block's tiletypes and its designations separately and sends
/// each only when its own hash has moved, so a block arrives with tiles and no
/// word about what has been seen, or with the seen bits and nothing to draw, as
/// often as it arrives whole. Reading a missing array as revealed would reopen
/// columns over rock nobody has been anywhere near.
fn seen_into(block: &rfr::MapBlock) -> Option<bool> {
    (block.hidden.len() >= TILES_PER_BLOCK)
        .then(|| block.hidden[..TILES_PER_BLOCK].iter().all(|&h| h))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMN: (i32, i32) = (4, -2);

    /// A reply about one column: levels from the top down, `true` where nobody
    /// has seen into the block.
    fn reply(column: (i32, i32), levels: &[(i32, bool)]) -> Vec<((i32, i32), i32, bool)> {
        levels.iter().map(|&(z, blind)| (column, z, blind)).collect()
    }

    fn nothing() -> HashSet<(i32, i32)> {
        HashSet::new()
    }

    #[test]
    fn a_reply_carrying_no_designations_says_nothing_about_a_block() {
        let mut block = rfr::MapBlock { tiles: vec![0; TILES_PER_BLOCK], ..Default::default() };
        assert_eq!(seen_into(&block), None, "tiles alone are not a claim about who has seen it");
        block.hidden = vec![true; TILES_PER_BLOCK];
        assert_eq!(seen_into(&block), Some(true));
        block.hidden[7] = false;
        assert_eq!(seen_into(&block), Some(false));
        // Half an array is not an answer either.
        block.hidden.truncate(TILES_PER_BLOCK - 1);
        assert_eq!(seen_into(&block), None);
    }

    #[test]
    fn the_first_blind_block_under_the_ground_closes_a_column() {
        let mut floors = Floors::default();
        let news = floors.settle(&reply(COLUMN, &[(120, false), (119, false), (118, true)]));
        assert_eq!(floors.stop(COLUMN), Some(118));
        assert!(news.is_empty(), "finding the ground is not the ground changing");
    }

    #[test]
    fn a_closed_column_is_asked_about_down_to_one_level_under_its_floor() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (118, true)]));
        for (z, open) in [(120, true), (118, true), (117, true), (116, false)] {
            assert_eq!(floors.open(COLUMN, z, Phase::Surface, &nothing()), open, "level {z}");
            assert_eq!(floors.open(COLUMN, z, Phase::Probe, &nothing()), !open, "probe {z}");
        }
    }

    #[test]
    fn the_floor_block_turning_visible_reopens_the_column() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (118, true)]));
        // The pass that follows: the floor block itself has been revealed.
        let news = floors.settle(&reply(COLUMN, &[(118, false), (117, false)]));
        assert_eq!(floors.stop(COLUMN), None, "no floor stands, so the next slab descends");
        assert_eq!(news, vec![Sighting { column: COLUMN, z: 117, opened: true }]);
    }

    #[test]
    fn ground_under_a_still_hidden_floor_reopens_the_column() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (118, true)]));
        // What the probe sees: the floor block as hidden as ever, and a tunnel
        // two levels under it.
        let news = floors.settle(&reply(COLUMN, &[(117, true), (116, false)]));
        assert_eq!(news, vec![Sighting { column: COLUMN, z: 116, opened: true }]);
        assert_eq!(floors.stop(COLUMN), None, "the probe keeps descending this column");
        // And the same probe, one slab lower, finds where it stops now.
        floors.settle(&reply(COLUMN, &[(115, true), (114, true)]));
        assert_eq!(floors.stop(COLUMN), Some(115));
    }

    #[test]
    fn a_column_the_probe_reopened_is_chased_down_by_that_probe() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (118, true)]));
        let news = floors.settle(&reply(COLUMN, &[(116, false)]));
        let chasing: HashSet<(i32, i32)> = news.iter().map(|s| s.column).collect();
        assert!(!floors.open(COLUMN, 110, Phase::Probe, &nothing()), "no floor, nothing to probe");
        assert!(floors.open(COLUMN, 110, Phase::Probe, &chasing), "the probe owes it a floor");
    }

    #[test]
    fn rock_arriving_under_a_standing_floor_does_not_move_it() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (118, true)]));
        // A probe reaching under the floor: all rock, nothing revealed.
        let news = floors.settle(&reply(COLUMN, &[(116, true), (115, true), (114, true)]));
        assert!(news.is_empty());
        assert_eq!(floors.stop(COLUMN), Some(118), "the floor stays where the ground put it");
    }

    #[test]
    fn ground_that_goes_dark_again_closes_the_column_where_it_was() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (118, true)]));
        floors.settle(&reply(COLUMN, &[(116, false)]));
        assert_eq!(floors.stop(COLUMN), None, "ground under the floor reopened it");
        // The same block, wholly hidden again: whatever was seen there is not
        // there now, and the column stops where it did.
        floors.settle(&reply(COLUMN, &[(116, true)]));
        assert_eq!(floors.stop(COLUMN), Some(116));
    }

    #[test]
    fn a_blind_block_over_ground_already_seen_is_an_overhang_not_a_floor() {
        let mut floors = Floors::default();
        floors.settle(&reply(COLUMN, &[(120, false), (110, false)]));
        // A later reply about a block inside the hillside, over the shaft.
        floors.settle(&reply(COLUMN, &[(115, true)]));
        assert_eq!(floors.stop(COLUMN), None, "closing here would lose the shaft under it");
        floors.settle(&reply(COLUMN, &[(109, true)]));
        assert_eq!(floors.stop(COLUMN), Some(109));
    }

    #[test]
    fn ground_under_a_neighbours_floor_is_chased_sideways() {
        let mut floors = Floors::default();
        // Three columns, floored at three depths.
        floors.settle(&reply((0, 0), &[(120, false), (118, true)]));
        floors.settle(&reply((1, 0), &[(120, false), (110, true)]));
        floors.settle(&reply((0, 1), &[(120, false), (109, true)]));
        // A tunnel is found in the middle column, well under all of them.
        let news = floors.settle(&reply((0, 0), &[(107, false)]));
        let mut sides = floors.spread(&news);
        sides.sort();
        assert_eq!(
            sides,
            vec![((0, 1), 107), ((1, 0), 107)],
            "both neighbours stop over the tunnel, so both get looked at"
        );
    }

    #[test]
    fn a_neighbour_that_reaches_the_level_anyway_is_left_alone() {
        let mut floors = Floors::default();
        floors.settle(&reply((0, 0), &[(120, false), (118, true)]));
        // This one already stops one level under the find, so its own pass
        // covers it.
        floors.settle(&reply((1, 0), &[(120, false), (108, true)]));
        let news = floors.settle(&reply((0, 0), &[(107, false)]));
        assert_eq!(floors.spread(&news), vec![], "107 >= 108 - 1: the surface pass has it");
    }

    #[test]
    fn the_first_sighting_of_a_column_spreads_nowhere() {
        let mut floors = Floors::default();
        floors.settle(&reply((1, 0), &[(120, false), (110, true)]));
        // The first pass over a column is the descent finding the ground, not
        // ground appearing, so the neighbours are not disturbed.
        let news = floors.settle(&reply((0, 0), &[(100, false)]));
        assert!(news.is_empty());
        assert!(floors.spread(&news).is_empty());
    }

    #[test]
    fn a_forced_pass_keeps_only_what_the_game_answered_with() {
        let asked =
            vec![(0, 0, 10), (0, 0, 9), (1, 0, 10), (1, 0, 9), (0, 0, 10)];
        let arrived = vec![(0, 0, 10), (1, 0, 9), (7, 7, 7)];
        assert_eq!(
            unanswered(&asked, &arrived),
            vec![(0, 0, 9), (1, 0, 10)],
            "asked and unanswered, each once, and nothing it never asked about"
        );
        assert!(unanswered(&[], &arrived).is_empty(), "an unforced pass asks nothing of the cache");
    }

    #[test]
    fn a_reply_is_placed_by_the_window_it_was_built_in() {
        // Origin pinned one region tile east of the world's corner.
        let origin = (48, 96, -29);
        let mut list = rfr::BlockList::default();
        assert_eq!(reply_frame(&list, origin, 3), None, "a reply that says nothing moves nothing");
        // The window where the pass began: the frame is zero.
        list.map_x = Some(1);
        list.map_y = Some(2);
        assert_eq!(reply_frame(&list, origin, 3), Some((0, 0, 3)));
        // The character crossed east mid-pass and the game re-centred: 48 tiles
        // on, which is three blocks.
        list.map_x = Some(2);
        assert_eq!(reply_frame(&list, origin, 3), Some((3, 0, 3)));
    }

    /// A synthetic window: nine block columns square, eight levels deep.
    fn window() -> BlockBounds {
        BlockBounds { min_x: 0, max_x: 9, min_y: 0, max_y: 9, min_z: 100, max_z: 108 }
    }

    /// The x or y span of a band, on whichever axis the cut was made.
    fn spans(bands: &[BlockBounds], across_x: bool) -> Vec<(i32, i32)> {
        bands
            .iter()
            .map(|b| if across_x { (b.min_x, b.max_x) } else { (b.min_y, b.max_y) })
            .collect()
    }

    #[test]
    fn nothing_travelling_leaves_the_window_in_one_piece() {
        assert_eq!(travel_bands(&window(), (4, 4), None), vec![window()]);
    }

    #[test]
    fn the_blocks_ahead_are_asked_for_before_the_sides_and_the_sides_before_behind() {
        let bands = travel_bands(&window(), (4, 4), Some((1.0, 0.0)));
        assert_eq!(spans(&bands, true), vec![(5, 9), (4, 5), (0, 4)]);
        // Walking west is the same cut read the other way round.
        let back = travel_bands(&window(), (4, 4), Some((-1.0, 0.0)));
        assert_eq!(spans(&back, true), vec![(0, 4), (4, 5), (5, 9)]);
        // North is negative y, so the blocks ahead are the low ones.
        let north = travel_bands(&window(), (4, 4), Some((0.0, -1.0)));
        assert_eq!(spans(&north, false), vec![(0, 4), (4, 5), (5, 9)]);
        for band in &bands {
            assert_eq!((band.min_y, band.max_y, band.min_z, band.max_z), (0, 9, 100, 108));
        }
    }

    #[test]
    fn a_diagonal_run_is_cut_across_the_axis_it_mostly_runs_on() {
        // South-east, leaning east: the cut is on x.
        let bands = travel_bands(&window(), (4, 4), Some((0.8, 0.6)));
        assert_eq!(spans(&bands, true), vec![(5, 9), (4, 5), (0, 4)]);
        // South-east, leaning south: the same run, cut on y.
        let bands = travel_bands(&window(), (4, 4), Some((0.6, 0.8)));
        assert_eq!(spans(&bands, false), vec![(5, 9), (4, 5), (0, 4)]);
    }

    #[test]
    fn the_bands_cover_the_window_exactly_once_and_never_widen_it() {
        let box_ = window();
        for heading in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.7, -0.7)] {
            for here in [(4, 4), (0, 0), (8, 8), (-3, 12)] {
                let bands = travel_bands(&box_, here, Some(heading));
                let mut seen = Vec::new();
                for band in &bands {
                    assert!(
                        band.min_x >= box_.min_x
                            && band.max_x <= box_.max_x
                            && band.min_y >= box_.min_y
                            && band.max_y <= box_.max_y
                            && band.min_z == box_.min_z
                            && band.max_z == box_.max_z,
                        "a band reached outside the window: {band:?}"
                    );
                    for bx in band.min_x..band.max_x {
                        for by in band.min_y..band.max_y {
                            seen.push((bx, by));
                        }
                    }
                }
                let whole = (box_.max_x - box_.min_x) * (box_.max_y - box_.min_y);
                seen.sort_unstable();
                let asked = seen.len();
                seen.dedup();
                assert_eq!(seen.len(), asked, "a column was asked about twice: {heading:?}");
                assert_eq!(asked as i32, whole, "the bands lost a column: {heading:?} at {here:?}");
            }
        }
    }

    #[test]
    fn going_down_into_a_column_forgets_its_floor() {
        let mut floors = Floors::default();
        for column in [(0, 0), (1, 0), (5, 5)] {
            floors.settle(&reply(column, &[(120, false), (118, true)]));
        }
        // Standing over the floors changes nothing.
        floors.forget_under((0, 0, 119));
        assert_eq!(floors.len(), 3);
        // Standing at one drops it and the ring around it, and leaves the far
        // column alone.
        floors.forget_under((0, 0, 118));
        assert_eq!(floors.stop((0, 0)), None);
        assert_eq!(floors.stop((1, 0)), None);
        assert_eq!(floors.stop((5, 5)), Some(118));
    }
}
