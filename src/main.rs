use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::process;

const TEX_FILE: &str = "sample.tex";
const DIVIDER: &str = "==========";

/// Pattern for the LaTeX \begin{NAME} command. The environment name (`NAME`)
/// is captured as group 1. Finding the matching \end{NAME} is delegated to a
/// manual parser (backreferences are not supported by Rust's `regex` crate).
/// Convention: every theorem environment ends with the letter 't'.
const BEGIN_ENV_PATTERN: &str = r"\\begin\{([A-Za-z]+)\}";

/// Pattern for the LaTeX \end{NAME} command. The environment name is captured
/// as group 1.
const END_ENV_PATTERN: &str = r"\\end\{([A-Za-z]+)\}";

/// Pattern for \section{...} commands, with an optional short title in [].
const SECTION_PATTERN: &str = r"\\section(?:\[[^\]]*\])?\{[^}]*\}";

const BEGIN_DOCUMENT: &str = r"\begin{document}";
const END_DOCUMENT: &str = r"\end{document}";

// ---------------------------------------------------------------------------
// Storyboard — scene data structures (Stage 2)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct Storyboard {
    version: String,
    title: String,
    scenes: Vec<Scene>,
}

/// Wrapper that encapsulates the polymorphic `Scene` inside the `scene`
/// property. The root schema is a simple flat object, while the polymorphism
/// (`anyOf` with `const` on `scene_type`) is nested under `scene`, so that
/// OpenAI Structured Outputs constraints are respected (no anyOf/const at the
/// top level).
#[derive(Debug, Serialize, Deserialize)]
struct SceneResponse {
    scene: Scene,
}

/// Content segment: either text, an equation, or an image. The order in
/// `Vec<Segment>` preserves the reading order of the LaTeX chunk, including
/// the position of `\includegraphics` blocks intermixed with text and math.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum Segment {
    Text {
        content: String,
    },
    Equation {
        latex: String,
    },
    Image {
        asset: String,
        caption: Option<String>,
    },
}

/// Polymorphic representation of a LaTeX chunk.
/// Each variant maps a block category with dedicated fields, while keeping
/// common fields (`id`, `title_block`, `ambient`, `mixed_content`).
/// `mixed_content` is a `Vec<Segment>` (text/equation) that preserves the
/// reading order of the LaTeX chunk. The `scene_type` tag enables Serde
/// polymorphism.
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "scene_type")]
enum Scene {
    Theorem {
        id: String,
        title_block: String,
        ambient: String,
        acronym: String,
        hypotheses: Vec<String>,
        thesis: String,
        mixed_content: Vec<Segment>,
    },
    Example {
        id: String,
        title_block: String,
        ambient: String,
        steps_explanation: Vec<String>,
        graphs: Vec<String>,
        mixed_content: Vec<Segment>,
    },
    Observation {
        id: String,
        title_block: String,
        ambient: String,
        mixed_content: Vec<Segment>,
    },
}

fn body_after_preamble(tex: &str) -> &str {
    tex.split(BEGIN_DOCUMENT)
        .nth(1)
        .map(|rest| rest.split(END_DOCUMENT).next().unwrap_or(rest))
        .unwrap_or(tex)
}

/// Deterministically classifies the scene kind from the LaTeX environment
/// acronym. Rules:
/// - ends with 't' (case-insensitive) -> "Theorem"
/// - "exx" -> "Example"
/// - "obs" -> "Observation"
/// - "section" -> "Observation" (narrative fallback)
/// - anything else -> "Observation"
fn scene_kind_from_acronym(acronym: &str) -> &'static str {
    let a = acronym.trim().to_lowercase();
    if a == "exx" {
        "Example"
    } else if a == "obs" {
        "Observation"
    } else if a.ends_with('t') {
        "Theorem"
    } else {
        "Observation"
    }
}

/// Extracts chunks from the LaTeX body in order of appearance.
/// Returns a list of tuples `(environment_acronym, chunk_raw)`:
/// - `environment_acronym` is the LaTeX environment name (e.g. "Rt", "Lgt",
///   "obs", "exx") captured dynamically from `\begin{NAME}...\end{NAME}`. For
///   `\section{...}` commands the fixed string "section" is used.
/// - `chunk_raw` is the verbatim LaTeX source of the block.
fn extract_chunks(tex: &str) -> Vec<(String, String)> {
    let body = body_after_preamble(tex);
    let mut matches: Vec<(usize, String, String)> = Vec::new();

    let begin_re = Regex::new(BEGIN_ENV_PATTERN).expect("valid begin env pattern");
    let end_re = Regex::new(END_ENV_PATTERN).expect("valid end env pattern");

    // Collect all positions of \begin{NAME} and \end{NAME} together with the
    // environment name. We match only the name (group 1) and then locate the
    // corresponding \end manually, handling nesting of environments with the
    // same name (backreferences are not supported by Rust's `regex` crate, so
    // name<->end matching is implemented by hand).
    //
    // Only TOP-LEVEL environments (those whose \begin was encountered while the
    // stack was empty) are emitted as chunks. Nested environments such as
    // \begin{equation} inside \begin{Rt}, or \begin{array} inside \begin{obs},
    // are skipped — their content is already part of the enclosing chunk.
    #[derive(Clone)]
    struct EnvMark {
        pos: usize,
        is_begin: bool,
        name: String,
        full_end: usize,
        top_level: bool, // true if the stack was empty when this \begin was pushed
    }

    let mut marks: Vec<EnvMark> = Vec::new();
    for caps in begin_re.captures_iter(body) {
        if let (Some(full), Some(name)) = (caps.get(0), caps.get(1)) {
            marks.push(EnvMark {
                pos: full.start(),
                is_begin: true,
                name: name.as_str().to_string(),
                full_end: full.end(),
                top_level: false,
            });
        }
    }
    for caps in end_re.captures_iter(body) {
        if let (Some(full), Some(name)) = (caps.get(0), caps.get(1)) {
            marks.push(EnvMark {
                pos: full.start(),
                is_begin: false,
                name: name.as_str().to_string(),
                full_end: full.end(),
                top_level: false,
            });
        }
    }
    marks.sort_by_key(|m| m.pos);

    // Stack to match each \begin{NAME} with the corresponding \end{NAME},
    // handling nesting of environments with the same name. Only environments
    // whose \begin was pushed onto an empty stack are emitted as chunks.
    let mut stack: Vec<EnvMark> = Vec::new();
    for mark in marks.iter() {
        if mark.is_begin {
            let top_level = stack.is_empty();
            let mut m = mark.clone();
            m.top_level = top_level;
            stack.push(m);
        } else {
            // Find the top-most \begin on the stack with the same name
            let mut matched_idx = None;
            for (i, b) in stack.iter().enumerate().rev() {
                if b.name == mark.name {
                    matched_idx = Some(i);
                    break;
                }
            }
            if let Some(i) = matched_idx {
                let b = stack.remove(i);
                if b.top_level {
                    let raw = &body[b.pos..mark.full_end];
                    matches.push((b.pos, b.name.clone(), raw.to_string()));
                }
            }
        }
    }

    let section_re = Regex::new(SECTION_PATTERN).expect("valid section pattern");
    for m in section_re.find_iter(body) {
        // Only emit \section{...} commands that are NOT contained within an
        // already-extracted top-level environment.
        let start = m.start();
        let inside = matches
            .iter()
            .any(|(pos, _, _)| *pos <= start && start <= *pos);
        if !inside {
            matches.push((m.start(), "section".to_string(), m.as_str().to_string()));
        }
    }

    matches.sort_by_key(|(pos, _, _)| *pos);
    matches.into_iter().map(|(_, env, raw)| (env, raw)).collect()
}

/// JSON Schema describing the Rust `Scene` enum (internally tagged via
/// `scene_type`). Uses `anyOf` with three objects discriminated by `scene_type`
/// (const), compatible with OpenAI Structured Outputs: every branch has
/// `additionalProperties: false` and the full `required` list.
fn scene_json_schema(scene_type: &str, scene_id: &str) -> serde_json::Value {
    // Sub-schema for a single `Segment`: `anyOf` between Text and Equation,
    // discriminated by `type` (const). Compatible with Structured Outputs:
    // each branch has `type: "object"`, `additionalProperties: false` and the
    // full `required` list (including the `type` tag field).
    let segmento_schema = json!({
        "anyOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "type": { "type": "string", "const": "Text" },
                    "content": { "type": "string" }
                },
                "required": ["type", "content"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "type": { "type": "string", "const": "Equation" },
                    "latex": { "type": "string" }
                },
                "required": ["type", "latex"]
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "type": { "type": "string", "const": "Image" },
                    "asset": { "type": "string" },
                    "caption": { "type": ["string", "null"] }
                },
                "required": ["type", "asset", "caption"]
            }
        ]
    });

    // Schema for the `mixed_content` array: an ordered list of variable length
    // of `Segment`. Reused across all `Scene` variants.
    let mixed_content_schema = json!({
        "type": "array",
        "items": segmento_schema
    });

    // Per-variant object schema. Only the branch matching the deterministically
    // computed `scene_type` is included in the top-level `anyOf`: this makes the
    // choice structural (the model cannot pick another variant), fixing the issue
    // where gpt-4o-2024-08-06 ignored the discursive constraint and defaulted to
    // "Observation".
    let teorema = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "scene_type": { "type": "string", "const": "Theorem" },
            "id": { "type": "string", "const": scene_id },
            "title_block": { "type": "string" },
            "ambient": { "type": "string" },
            "acronym": { "type": "string" },
            "hypotheses": { "type": "array", "items": { "type": "string" } },
            "thesis": { "type": "string" },
            "mixed_content": mixed_content_schema
        },
        "required": [
            "scene_type", "id", "title_block", "ambient",
            "acronym", "hypotheses", "thesis", "mixed_content"
        ]
    });

    let esempio = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "scene_type": { "type": "string", "const": "Example" },
            "id": { "type": "string", "const": scene_id },
            "title_block": { "type": "string" },
            "ambient": { "type": "string" },
            "steps_explanation": { "type": "array", "items": { "type": "string" } },
            "graphs": { "type": "array", "items": { "type": "string" } },
            "mixed_content": mixed_content_schema
        },
        "required": [
            "scene_type", "id", "title_block", "ambient",
            "steps_explanation", "graphs", "mixed_content"
        ]
    });

    let osservazione = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "scene_type": { "type": "string", "const": "Observation" },
            "id": { "type": "string", "const": scene_id },
            "title_block": { "type": "string" },
            "ambient": { "type": "string" },
            "mixed_content": mixed_content_schema
        },
        "required": [
            "scene_type", "id", "title_block", "ambient",
            "mixed_content"
        ]
    });

    let branch = match scene_type {
        "Example" => esempio,
        "Observation" => osservazione,
        _ => teorema,
    };

    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "scene": {
                "anyOf": [ branch ]
            }
        },
        "required": ["scene"]
    })
}

/// Calls the official gpt-4o-2024-08-06 API to generate a `Scene` from a LaTeX chunk,
/// leveraging native Structured Outputs to guarantee the formatting.
async fn generate_scene_json(
    chunk: &str,
    acronym: &str,
    scene_id: &str,
) -> Result<Scene, Box<dyn std::error::Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY not set in the environment")?;
    let endpoint = "https://api.openai.com/v1/chat/completions";

    let scene_type = scene_kind_from_acronym(acronym);
    let system_prompt = format!(
        "Sei un convertitore rigoroso da LaTeX a JSON educativo. \
        Dato un chunk LaTeX, produci ESCLUSIVAMENTE un oggetto JSON che aderisca \
        STRETTAMENTE allo schema JSON fornito (enum Scene internamente taggato via \
        'scene_type'). \
        VINCOLO DETERMINISTICO SUL scene_type: il scene_type DEVE essere esattamente \
        '{scene_type}' (pre-calcolato deterministicamente lato Rust in base alla \
        regola: nome ambient LaTeX che termina con 't' -> Theorem; 'exx' -> Example; \
        'obs' -> Observation). NON cambiare scene_type, NON usare 'Observation' \
        se la regola dice 'Theorem'. \
        Regole d'oro: \
        - Estrema COERENZA con il materiale LaTeX fornito: ogni campo deve derivare \
          ESCLUSIVAMENTE dal content del chunk. \
        - DIVIETO ASSOLUTO di allucinazioni: NON aggiungere testi, nomi propri, \
          titoli, autori, contesti, commenti o dati non presenti nel sorgente LaTeX. \
          Se un'informazione non e scritta nel chunk, NON inserirla. \
        - 'title_block': ricavalo SOLO da testo esplicito nel chunk (es. argomento \
          opzionale \\begin{{exx}}[...] o comando \\section{{...}}). Se il chunk NON \
          contiene alcun titolo esplicito, usa una stringa vuota \"\" oppure il \
          nome dell'ambient LaTeX (es. \"Rt\"). VIETATO inventare nomi di teoremi \
          (es. \"Theorem di Bolzano\", \"Theorem di Rolle\") se non compaiono \
          letteralmente nel sorgente LaTeX. \
        - 'mixed_content': array ORDINATO di lunghezza variabile che ricostruisce \
          fedelmente la sequenza di lettura del chunk LaTeX, alternando tre tipi di \
          frammento: testo (type='Text', campo 'content'), equazioni \
          (type='Equation', campo 'latex') e immagini (type='Image', campo 'asset' \
          con il path verbatim tratto da \\includegraphics{{...}} e campo 'caption' \
          con la didascalia se presente nel chunk, altrimenti null). \
          PER IL CAMPO 'latex' DELLE EQUAZIONI: estrai SOLO il corpo matematico \
          NUDO, senza delimitatori (nessun $$, nessun \\[, nessun \\], nessun \\(, \
          nessun \\), nessun \\begin{{equation}}...\\end{{equation}}). Ad esempio, \
          se nel chunk compare '$$f(x) = x^2$$' il campo 'latex' deve contenere \
          ESATTAMENTE 'f(x) = x^2'. Se compare '\\begin{{equation}}\\exists c \
          \\in (0,1)...\\end{{equation}}', il campo 'latex' deve contenere \
          'ESATTAMENTE '\\exists c \\in (0,1)...'. Non inserire newline superflui. \
        - 'hypotheses' e 'thesis' (Theorem) devono riflettere fedelmente l'enunciato \
          presente nel chunk, senza nominare il teorema se non e citato. \
        - 'acronym' (Theorem): assume ESATTAMENTE il nome dell'ambient LaTeX che \
          racchiude il chunk, cosi come compare dopo \\begin{{...}} (es. 'Rt', 'Lgt'). \
          NON tradurre, espandere o modificare l'acronym. \
        - 'steps_explanation' (Example) riporta i passaggi deducibili dal chunk. \
        - 'graphs' (Example) riporta i path/asset delle \\includegraphics presenti. \
        - For Observation scenes, the discourse text is expressed entirely through \
          'mixed_content' (Text segments). Do not include a separate notes field. \
        - 'id' e gia assegnato deterministicamente (const nello schema): usa \
          ESATTAMENTE il valore fornito, non modificarlo. \
        - Non includere testo fuori dal JSON. Non usare markdown. \
        Restituisci ESCLUSIVAMENTE l'oggetto JSON conforme allo schema."
    );

    let user_content = format!("Ambiente LaTeX (acronym): {acronym}\n\n{chunk}");

    let payload = json!({
        "model": "gpt-4o-2024-08-06",
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_content }
        ],
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "scene",
                "strict": true,
                "schema": scene_json_schema(scene_type, scene_id)
            }
        }
    });

    let client = reqwest::Client::new();
    let response = client
        .post(endpoint)
        .bearer_auth(&api_key)
        .json(&payload)
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("gpt-4o-2024-08-06 request failed ({status}): {text}").into());
    }

    let resp_json: serde_json::Value = response.json().await?;
    let content = resp_json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("response missing choices[0].message.content")?;

    let scene_response: SceneResponse = serde_json::from_str(content)?;
    Ok(scene_response.scene)
}

/// Returns the `id` of any `Scene` variant.
fn scene_id_of(s: &Scene) -> &str {
    match s {
        Scene::Theorem { id, .. }
        | Scene::Example { id, .. }
        | Scene::Observation { id, .. } => id,
    }
}

/// Returns the length of `mixed_content` of any `Scene` variant.
fn scene_mixed_content_len(s: &Scene) -> usize {
    match s {
        Scene::Theorem { mixed_content, .. }
        | Scene::Example { mixed_content, .. }
        | Scene::Observation { mixed_content, .. } => mixed_content.len(),
    }
}

/// Returns the `ambient` (LaTeX environment name) of any `Scene` variant.
fn scene_ambient_of(s: &Scene) -> &str {
    match s {
        Scene::Theorem { ambient, .. }
        | Scene::Example { ambient, .. }
        | Scene::Observation { ambient, .. } => ambient,
    }
}

/// Sanitizes a LaTeX equation string for Manim consumption:
/// - Strips `$$...$$`, `\[...\]`, `\(...\)` delimiters.
/// - Strips `\begin{equation}...\end{equation}` wrappers (keeps inner math).
/// - Collapses newlines and excess whitespace into single spaces.
/// - Trims leading/trailing whitespace.
/// The result is a bare LaTeX math expression suitable for `MathTex`.
fn sanitize_equation(latex: &str) -> String {
    let mut s = latex.trim().to_string();

    // Strip $$ delimiters (display math).
    s = s.trim_start_matches("$$").trim_end_matches("$$").to_string();

    // Strip \[ \] delimiters.
    s = s.trim_start_matches("\\[").trim_end_matches("\\]").to_string();

    // Strip \( \) delimiters (inline math).
    s = s.trim_start_matches("\\(").trim_end_matches("\\)").to_string();

    // Strip \begin{equation}...\end{equation} wrapper (keep inner content).
    if s.contains("\\begin{equation}") {
        if let Some(start) = s.find("\\begin{equation}") {
            let after_begin = &s[start + "\\begin{equation}".len()..];
            if let Some(end) = after_begin.find("\\end{equation}") {
                s = after_begin[..end].to_string();
            }
        }
    }

    // Collapse newlines and excess whitespace into a single space.
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");

    s
}

/// Applies `sanitize_equation` to every `Equation` segment inside a scene.
fn sanitize_scene_segments(scene: &mut Scene) {
    let mixed = match scene {
        Scene::Theorem { mixed_content, .. }
        | Scene::Example { mixed_content, .. }
        | Scene::Observation { mixed_content, .. } => mixed_content,
    };
    for seg in mixed.iter_mut() {
        if let Segment::Equation { latex } = seg {
            *latex = sanitize_equation(latex);
        }
    }
}

/// Lightweight validation of the generated scenes. Returns a list of errors;
/// an empty vec means the storyboard is well-formed. Checks performed:
/// 1. Scene IDs are unique and non-empty across the whole storyboard.
/// 2. Each scene has a non-empty `mixed_content`.
fn validate_storyboard(scenes: &[Scene], _chunk_count: usize) -> Vec<String> {
    let mut errors = Vec::new();

    // Check 1: unique non-empty IDs.
    let mut seen_ids = std::collections::HashSet::new();
    for (i, s) in scenes.iter().enumerate() {
        let id = scene_id_of(s);
        if id.trim().is_empty() {
            errors.push(format!("scene #{i} has empty id"));
            continue;
        }
        if seen_ids.contains(id) {
            errors.push(format!("duplicate scene id: '{id}'"));
        } else {
            seen_ids.insert(id.to_string());
        }
    }

    // Check 2: non-empty mixed_content per scene, except for \section
    // header scenes (ambient == "section") which legitimately have no body.
    for (i, s) in scenes.iter().enumerate() {
        if scene_mixed_content_len(s) == 0 && scene_ambient_of(s) != "section" {
            errors.push(format!(
                "scene #{i} (id='{}') has empty mixed_content",
                scene_id_of(s)
            ));
        }
        if scene_mixed_content_len(s) == 0 && scene_ambient_of(s) == "section" {
            // Header scene: this is acceptable, no error.
        }
    }

    errors
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let tex = fs::read_to_string(TEX_FILE).unwrap_or_else(|e| {
        eprintln!("Error reading {TEX_FILE}: {e}");
        process::exit(1);
    });

    let chunks = extract_chunks(&tex);

    println!("Found {} chunks.\n", chunks.len());

    for (i, (acronym, chunk)) in chunks.iter().enumerate() {
        if i > 0 {
            println!("\n{DIVIDER}\n");
        }
        println!("[{acronym}]\n{chunk}");
    }

    if chunks.is_empty() {
        eprintln!("No chunks extracted from {TEX_FILE}.");
        return;
    }

    let mut generated_scenes: Vec<Scene> = Vec::new();

    // Deterministic, stable scene identifier per acronym: a counter is
    // incremented each time the same acronym appears (e.g. "Rt-0", "Lg-0",
    // "exx-0", "exx-1", "obs-0", "obs-1"). The resulting id is passed to the
    // LLM as a `const` inside the structured-output schema, so the model
    // cannot alter it. IDs remain stable across runs regardless of LLM
    // nondeterminism, which is essential for cross-scene references.
    let mut acronym_counters: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for (acronym, chunk) in &chunks {
        let count = acronym_counters.entry(acronym.clone()).or_insert(0);
        let scene_id = format!("{acronym}-{count}");
        *count += 1;

        println!("\n{DIVIDER}");
        println!("Generating scene with gpt-4o-2024-08-06 for chunk [{acronym}] (id={scene_id})...");
        println!("{DIVIDER}\n");

        match generate_scene_json(chunk, acronym, &scene_id).await {
            Ok(scene) => {
                println!("Scene generated successfully.");
                generated_scenes.push(scene);
            }
            Err(e) => {
                eprintln!("Error while generating the scene for [{acronym}]: {e}");
                continue;
            }
        }
    }

    if generated_scenes.len() != chunks.len() {
        eprintln!(
            "Warning: generated {} scenes out of {} chunks (some chunks may have failed).",
            generated_scenes.len(),
            chunks.len()
        );
    }

    // Sanitize all Equation segments: strip LaTeX delimiters, newlines, and
// \begin{equation} wrappers so the stored `storyboard.json` is ready for
// Manim's MathTex (no $$, no \[\], no \(\), no blank lines).
    for scene in generated_scenes.iter_mut() {
        sanitize_scene_segments(scene);
    }

    let validation_errors = validate_storyboard(&generated_scenes, chunks.len());
    if !validation_errors.is_empty() {
        eprintln!("Storyboard validation failed:");
        for err in &validation_errors {
            eprintln!("  - {err}");
        }
        process::exit(1);
    }

    let storyboard = Storyboard {
        version: "1.0".into(),
        title: "Script".into(),
        scenes: generated_scenes,
    };
    let json = serde_json::to_string_pretty(&storyboard)
        .expect("JSON serialization failed");
    std::fs::write("storyboard.json", &json).unwrap_or_else(|e| {
        eprintln!("Error writing storyboard.json: {e}");
        process::exit(1);
    });
    println!("\nStoryboard saved to storyboard.json");
}