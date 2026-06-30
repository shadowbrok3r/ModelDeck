//! Client+server data model. Re-exports the shared domain types and adds a few
//! display helpers used only by the UI.

pub use modeldeck_shared::*;

/// Human-readable byte size, e.g. 154_000_000_000 -> "143.4 GB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// mdi icon name for an engine, used on profile chips.
pub fn engine_icon(engine: &EngineKind) -> &'static str {
    match engine {
        EngineKind::LlamaCpp => "🦙",
        EngineKind::Vllm => "⚡",
        EngineKind::Ollama => "🐘",
        EngineKind::TabbyApi => "🐈",
        EngineKind::Custom => "🧩",
    }
}

pub const ALL_ENGINES: [EngineKind; 5] = [
    EngineKind::LlamaCpp,
    EngineKind::Vllm,
    EngineKind::Ollama,
    EngineKind::TabbyApi,
    EngineKind::Custom,
];

pub fn engine_from_label(s: &str) -> EngineKind {
    match s {
        "llama.cpp" => EngineKind::LlamaCpp,
        "vllm" => EngineKind::Vllm,
        "ollama" => EngineKind::Ollama,
        "tabbyAPI" => EngineKind::TabbyApi,
        _ => EngineKind::Custom,
    }
}
