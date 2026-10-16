// Allow this DLL smoke test to run independently of other plugins' test modules.
#[path = "../src/fixtures.rs"]
mod smoke_fixtures;

#[path = "../src/stats_smoke.rs"]
mod stats_smoke;
