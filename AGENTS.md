# Repository Guidance

- The intended product is an automated, scalable pipeline from LaTeX files to Manim code and rendered MP4 videos in a 3Blue1Brown-style format; this repository currently covers only LaTeX chunk extraction and OpenAI storyboard generation.
- This is a single Rust 2024 binary; the application entrypoint and parser/API workflow are in `src/main.rs`.
- Run Cargo commands from the repository root. The binary uses hard-coded relative paths: it reads `sample.tex` and writes `storyboard.json`.
- `cargo fmt --check` checks formatting; `cargo check` compiles the binary; `cargo test` is the available test command, but the repository currently has no tests.
- `cargo run` is not a local-only smoke test: it loads `.env` via `dotenvy`, requires `OPENAI_API_KEY`, sends one OpenAI request per extracted chunk using `gpt-4o-2024-08-06`, and overwrites `storyboard.json`.
- Do not commit or expose `.env`; it is ignored and contains runtime credentials.
- Chunk extraction emits top-level LaTeX environments and `\section` commands in source order; nested environments stay inside their parent chunk. Environment names ending in `t` classify as `Theorem`, `exx` as `Example`, and `obs` or anything else as `Observation`.
- Scene IDs are deterministic acronym counters such as `Rt-0` and `exx-1`; preserve this scheme when changing generation or downstream references.
