//! Method descriptors for the RPCs this project uses.
//!
//! Names match the `// RPC` comments in the vendored `.proto` files; the message
//! names are the fully-qualified protobuf names the server binds against.

use crate::Method;

const PLUGIN: &str = "RemoteFortressReader";
const EMPTY: &str = "dfproto.EmptyMessage";

/// Runs a DFHack console command. Bound at the fixed core id, not by name.
pub const RUN_COMMAND: Method =
    Method::core("RunCommand", "dfproto.CoreRunCommandRequest", EMPTY);

macro_rules! rfr {
    ($konst:ident, $name:literal, $input:expr, $output:expr) => {
        pub const $konst: Method = Method::plugin(PLUGIN, $name, $input, $output);
    };
}

rfr!(GET_VERSION_INFO, "GetVersionInfo", EMPTY, "RemoteFortressReader.VersionInfo");
rfr!(GET_MAP_INFO, "GetMapInfo", EMPTY, "RemoteFortressReader.MapInfo");
rfr!(GET_TILETYPE_LIST, "GetTiletypeList", EMPTY, "RemoteFortressReader.TiletypeList");
rfr!(GET_MATERIAL_LIST, "GetMaterialList", EMPTY, "RemoteFortressReader.MaterialList");
rfr!(GET_GROWTH_LIST, "GetGrowthList", EMPTY, "RemoteFortressReader.MaterialList");
rfr!(GET_ITEM_LIST, "GetItemList", EMPTY, "RemoteFortressReader.MaterialList");
rfr!(GET_UNIT_LIST, "GetUnitList", EMPTY, "RemoteFortressReader.UnitList");
rfr!(GET_VIEW_INFO, "GetViewInfo", EMPTY, "RemoteFortressReader.ViewInfo");
rfr!(GET_PAUSE_STATE, "GetPauseState", EMPTY, "RemoteFortressReader.SingleBool");
rfr!(GET_WORLD_MAP_CENTER, "GetWorldMapCenter", EMPTY, "RemoteFortressReader.WorldMap");
rfr!(GET_WORLD_MAP, "GetWorldMap", EMPTY, "RemoteFortressReader.WorldMap");
rfr!(GET_WORLD_MAP_NEW, "GetWorldMapNew", EMPTY, "RemoteFortressReader.WorldMap");
rfr!(GET_BUILDING_DEF_LIST, "GetBuildingDefList", EMPTY, "RemoteFortressReader.BuildingList");
rfr!(GET_CREATURE_RAWS, "GetCreatureRaws", EMPTY, "RemoteFortressReader.CreatureRawList");
rfr!(GET_PLANT_RAWS, "GetPlantRaws", EMPTY, "RemoteFortressReader.PlantRawList");
rfr!(RESET_MAP_HASHES, "ResetMapHashes", EMPTY, EMPTY);

rfr!(
    GET_BLOCK_LIST,
    "GetBlockList",
    "RemoteFortressReader.BlockRequest",
    "RemoteFortressReader.BlockList"
);
rfr!(
    GET_PLANT_LIST,
    "GetPlantList",
    "RemoteFortressReader.BlockRequest",
    "RemoteFortressReader.PlantList"
);
rfr!(
    GET_UNIT_LIST_INSIDE,
    "GetUnitListInside",
    "RemoteFortressReader.BlockRequest",
    "RemoteFortressReader.UnitList"
);
rfr!(GET_REGION_MAPS, "GetRegionMaps", EMPTY, "RemoteFortressReader.RegionMaps");
rfr!(GET_REGION_MAPS_NEW, "GetRegionMapsNew", EMPTY, "RemoteFortressReader.RegionMaps");
