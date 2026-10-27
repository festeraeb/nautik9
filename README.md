# 🐾 NautiK9 Survey & Personal Organization Engine

High-performance, pure-Rust alternative to WebAssembly-heavy vault apps, built from the ground up for the **Samsung Galaxy S25 Ultra (Snapdragon 8 Elite / One UI 7)** and federated with the **CesarOps Swarm & Lemonade Cluster**.

---

## 🌟 Why NautiK9?

Previous mobile vault implementations (e.g., Jade Vault) relied on running three separate Emscripten WebAssembly engines inside an Android WebView, resulting in:
- High memory usage (>500 MB)
- 3–8 second UI freezes during client-side JavaScript embedding
- Suboptimal hardware acceleration

**NautiK9 replaces this with a native, pure-Rust modular architecture:**
- **Zero-lag embedded vector search (<15ms)** via native SIMD / ARM NEON.
- **Embedded Wayfinder Knowledge Vault**: Local SQLite-backed storage for survey observations, personal notes, and mission logs with semantic search and automatic 8-step task decomposition.
- **Symmetrical Lemonade Client**: Dynamic dual-mode LLM reasoning. Streams online via SSE to `https://cesarops.com/lemonade/v1` when connected, and drops down to on-device quantized models when offline with zero UI stalls or feature loss.
- **Hardware Generational Profiling**: Tailored for the **Samsung Galaxy S25 Ultra** (Snapdragon 8 Elite / Oryon ARMv9.2 SVE2, Hexagon NPU, Adreno 830 Vulkan 1.3, One UI 7 Galaxy AI integration), with graceful scaffolding across older phone generations.
- **Mack & Survey Boat Companion**: Ambient reactive avatar engine featuring Mack (the survey dog) and the NautiK9 vessel with the flag, transitioning between `Idle`, `Attentive`, `Sniffing`, `TargetLocked`, and `SurveyNavigating`.

---

## 🏗️ Workspace Architecture

```
nautik9/
├── Cargo.toml                      # Unified workspace manifest
├── crates/
│   ├── nautik9-core/               # Module system trait, event bus, hardware profiler, Mack avatar
│   ├── nautik9-wayfinder/          # Embedded Wayfinder vault, SQLite storage, semantic search, decomposer
│   ├── nautik9-lemonade/           # Symmetrical Lemonade client & offline fallback engine
│   ├── nautik9-fleet/              # CesarOps swarm bridge, Buzz board reporter, telemetry
│   └── nautik9-app/                # CLI runtime & application shell
```

---

## 📱 Hardware Profiling & Samsung Galaxy S25 Ultra / One UI 7

NautiK9 dynamically profiles the host device to allocate appropriate model sizes and vector dimensions:

| Dimension | Flagship Ultra (Samsung S25 Ultra) | Modern High-End (S24 Ultra) | Baseline ARM64 |
| :--- | :--- | :--- | :--- |
| **SoC / CPU** | Snapdragon 8 Elite / Oryon (ARMv9.2 SVE2) | Snapdragon 8 Gen 3 (ARM NEON) | Generic Cortex-A53 |
| **RAM** | 16,384 MB LPDDR5X | 12,288 MB | 4,096 MB |
| **NPU / GPU** | Hexagon HTP (45 TOPS) + Adreno 830 Vulkan | Hexagon NPU + Adreno 750 | CPU scalar only |
| **LLM Tier** | 7B / 8B Quantized (IQ4_XS) | 3B Quantized (Q4_K_M) | 0.5B / 1.5B (Q4_0) |
| **Embeddings** | 768-dimensional (embeddinggemma-300m) | 768-dimensional | 384-dimensional |
| **One UI 7 Tools** | Edge Panel, S-Pen Air Actions, Galaxy AI Bridge | One UI 6.1.1 basic | None |

---

## 🛠️ CLI Usage & Quickstart

### 1. View System & Hardware Profile Status
```bash
cargo run -p nautik9-app -- --device s25ultra status
```

### 2. Index a New Field Note or Survey Observation
```bash
cargo run -p nautik9-app -- --device s25ultra index "note-001" \
  "First NautiK9 Survey Pass" \
  "Mack spotted magnetic anomaly near flag buoy at coordinates 45.02N 83.21W." \
  -t sonar -t mack -t survey
```

### 3. Semantic Search with Reactive Mack Companion
```bash
cargo run -p nautik9-app -- --device s25ultra search "flag buoy sonar"
```

### 4. Symmetrical Lemonade Chat (Online or Offline Fallback)
```bash
# Symmetrical cloud streaming or local on-device fallback
cargo run -p nautik9-app -- --device s25ultra chat "Status of the survey boat and Mack" --offline
```

### 5. Task Decomposition (Breaks big goals into 8 concrete steps)
```bash
cargo run -p nautik9-app -- --device s25ultra decompose "Multi-beam sonar pass on shallow shipwreck"
```

---

## 🧪 Running Automated Tests

All modules include unit tests verifying event routing, SQLite indexing, and hardware profiling:

```bash
cargo test --workspace
```
*(All 8 unit tests pass with zero warnings)*
