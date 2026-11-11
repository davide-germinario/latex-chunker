# latex-chunker

A Rust CLI tool that parses mathematical LaTeX documents into top-level semantic chunks and converts them into a validated, deterministic JSON storyboard designed for downstream [Manim](https://www.manim.community/) video generation.

> **Project Status — Work in Progress (Stage 1 & 2 complete)**  
> This repository implements the first two stages of an automated **LaTeX → Storyboard JSON → Manim Video** pipeline:
> - ✅ **Stage 1 (Implemented):** Deterministic LaTeX chunk extraction and scene classification.
> - ✅ **Stage 2 (Implemented):** LLM-assisted extraction into a strict polymorphic JSON storyboard (`storyboard.json`) with equation sanitization for Manim's `MathTex`.
> - 🚧 **Stage 3 (Planned):** Downstream Python/Manim code generator and video renderer consuming `storyboard.json`.

---

## How It Works

### Stage 1: Deterministic LaTeX Chunking
1. **Document Body Isolation:** Strips the LaTeX preamble and extracts content strictly inside `\begin{document}...\end{document}`.
2. **Stack-Based Environment Parser:** Because Rust's `regex` crate does not support backreferences (needed to match `\begin{NAME}...\end{NAME}`), `extract_chunks` implements a manual stack-based matcher.
3. **Top-Level Filtering:** Only top-level environments and standalone `\section{...}` commands are emitted in reading order. Nested environments (such as `\begin{equation}` or `\begin{array}` inside a theorem) remain part of their enclosing chunk.
4. **Deterministic Classification & IDs:** Each chunk is classified before calling the LLM based on its LaTeX environment name:
   - Environment ending in `t` (e.g. `Rt`, `Lgt`) → `Theorem`
   - `exx` → `Example`
   - `obs` or `section` → `Observation`
   - Scene IDs are assigned deterministically with per-acronym counters (`Rt-0`, `exx-0`, `obs-1`, etc.) so cross-scene references remain stable across runs.

### Stage 2: Structured Storyboard Generation
1. **Polymorphic Data Model:** Scenes are modeled in Rust using internally tagged Serde enums (`#[serde(tag = "scene_type")] enum Scene`) containing an ordered `mixed_content: Vec<Segment>` (`Text`, `Equation`, `Image`) that preserves the exact reading sequence of the original LaTeX block.
2. **Schema-Enforced OpenAI Structured Outputs:** For each chunk, the tool builds a strict JSON Schema (`scene_json_schema`) and calls `gpt-4o-2024-08-06`. Both `scene_type` and `id` are injected as schema-level `const` constraints so the model cannot alter the deterministic classification or ID.
3. **Manim Equation Sanitization:** `sanitize_equation` strips LaTeX display/inline delimiters (`$$`, `\[...\]`, `\(...\)`, `\begin{equation}...\end{equation}`) and normalizes whitespace so every `Equation` segment is ready to be passed directly to Manim's `MathTex`.
4. **Storyboard Validation:** `validate_storyboard` verifies ID uniqueness and ensures non-section scenes contain non-empty `mixed_content` before writing `storyboard.json`.

---

## Example Output (`storyboard.json`)

```json
{
  "version": "1.0",
  "title": "Script",
  "scenes": [
    {
      "scene_type": "Theorem",
      "id": "Rt-0",
      "title_block": "",
      "ambient": "Rt",
      "acronym": "Rt",
      "hypotheses": [
        "Let a,b\\in \\mathbb R, a < b.",
        "If f:[a,b]\\to\\mathbb R is a continuous function on the closed interval [a,b], differentiable on the open interval (a,b) and f(a)=f(b),"
      ],
      "thesis": "\\exists c\\in (a,b) \\quad s.t. \\quad f'(c)= 0.",
      "mixed_content": [
        { "type": "Text", "content": "Let " },
        { "type": "Equation", "latex": "a,b\\in \\mathbb R" }
      ]
    }
  ]
}
```

---

## Getting Started

### Prerequisites
- **Rust** (Edition 2024 / Cargo)
- An **OpenAI API key** with access to `gpt-4o-2024-08-06`

### Usage
1. Create a `.env` file in the repository root:
   ```env
   OPENAI_API_KEY=sk-...
   ```
2. Place your input LaTeX file as `sample.tex` in the root directory.
3. Run the pipeline:
   ```bash
   cargo check
   cargo run
   ```
   The generated storyboard will be written to `storyboard.json`.

---

## Current Limitations & Next Steps
- **CLI Arguments:** Input (`sample.tex`) and output (`storyboard.json`) paths are currently constants in `src/main.rs`; next step is adding CLI flags (e.g. via `clap`) and a `--dry-run` mode to run Stage 1 chunking without making API calls.
- **Unit Tests:** Adding unit tests for `extract_chunks`, `scene_kind_from_acronym`, and `sanitize_equation`.
- **Stage 3 (Manim Renderer):** Building the Python consumer that maps `Theorem`, `Example`, and `Observation` scenes from `storyboard.json` into Manim scenes.
