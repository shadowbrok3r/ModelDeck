-- ModelDeck SurrealDB schema. Run once against the shared instance as root:
--   surreal sql --endpoint wss://surrealdb.shadowbroker.app --username root \
--     --password <pass> --pretty < db/schema.surql
-- Lives in its own namespace alongside the other home-assistant apps.

DEFINE NAMESPACE IF NOT EXISTS jarvis;
USE NS jarvis;
DEFINE DATABASE IF NOT EXISTS modeldeck;
USE NS jarvis DB modeldeck;

-- Schemaless: the hub writes records via typed SurrealValue rows. Tables are
-- declared so indexes can hang off them and the data model is documented.
DEFINE TABLE IF NOT EXISTS vm_target SCHEMALESS;   -- one per managed VM (key = slug id)
DEFINE TABLE IF NOT EXISTS profile    SCHEMALESS;   -- saved known-good service profiles
DEFINE TABLE IF NOT EXISTS active     SCHEMALESS;   -- active profile pointer (key = vm id)

-- Speed up the per-VM profile listing.
DEFINE INDEX IF NOT EXISTS profile_vm ON profile FIELDS vm;
