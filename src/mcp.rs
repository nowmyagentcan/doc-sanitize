//! The MCP server on stdio: `check_document` is read-only and `clean_document` writes copies;
//! both answer with the CLI's `--json` object.

use std::path::{self, Path, PathBuf};
use std::sync::Arc;

use doc_sanitize::input::{self, Input};
use doc_sanitize::report::{CHECKS, CleanReport, Error, Report};
use doc_sanitize::run::{self, CleanOptions, Options};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use rmcp::service::ServerInitializeError;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::generate::SchemaSettings;
use schemars::transform::RecursiveTransform;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckArgs {
    /// Files or folders; relative paths resolve against the server's working directory
    #[schemars(length(min = 1, max = 100))]
    paths: Vec<String>,
    /// Look in subfolders too
    #[serde(default)]
    recursive: bool,
    /// PDF user password
    password: Option<String>,
    /// Put the hidden text itself in each finding
    #[serde(default)]
    show_values: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CleanArgs {
    /// Files or folders; relative paths resolve against the server's working directory
    #[schemars(length(min = 1, max = 100))]
    paths: Vec<String>,
    /// Checks to leave in place
    #[serde(default)]
    #[schemars(schema_with = "keepable")]
    keep: Vec<String>,
    /// Where to write the copy; only with one path
    output: Option<String>,
    /// Folder for the copies, made as needed
    out_dir: Option<String>,
    /// Replace an existing output file
    #[serde(default)]
    overwrite: bool,
    /// Look in subfolders too
    #[serde(default)]
    recursive: bool,
    /// PDF user password
    password: Option<String>,
    /// Put the hidden text itself in what the re-check finds
    #[serde(default)]
    show_values: bool,
}

/// `keep`: every check id but the two a clean always removes.
fn keepable(_: &mut SchemaGenerator) -> Schema {
    let ids = CHECKS.iter().map(|c| c.0);
    let ids: Vec<_> = ids
        .filter(|id| !["signatures", "old-revisions"].contains(id))
        .collect();
    json_schema!({"type": "array", "items": {"enum": ids}, "default": []})
}

#[derive(Clone)]
struct Server;

#[tool_router]
impl Server {
    #[tool(
        description = "Check local PDF, Word (.docx), Excel (.xlsx) and PowerPoint (.pptx) files for hidden data before they are shared: document properties and author names, comments, tracked changes (including deleted text), hidden text, sheets, rows, slides and objects, speaker notes, embedded files, custom XML, macros and scripts, links to local files, picture metadata such as GPS, cropped picture areas, pivot caches, earlier PDF revisions, and fake redactions (text still under black boxes). Use it to verify whether a PDF redaction is real. Read-only. Returns every check that ran with its result; the hidden text itself is left out unless show_values is true. A file that can't be read gets its own error entry; on invalid arguments the result has isError true and one text block holding {\"error\":{\"code\",\"message\",\"hint\"}}.",
        input_schema = input_schema::<CheckArgs>(),
        output_schema = schema::<Report<'static>>(SchemaSettings::draft2020_12().for_serialize()),
        annotations(
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn check_document(&self, args: JsonObject) -> CallToolResult {
        answer(parse(args).and_then(check))
    }

    #[tool(
        description = "Write a cleaned copy of local PDF, DOCX, XLSX or PPTX files with hidden data removed (properties, comments, tracked changes accepted, hidden content, speaker notes, embedded files, macros, picture metadata, fake redactions: pages with hidden text become pictures, and more), then re-check the copy and save it only if nothing removable is left. The original file is never changed. keep lists check ids to leave in place. Returns what was removed, side effects such as formulas replaced by their values, the output path and the re-check.",
        input_schema = input_schema::<CleanArgs>(),
        output_schema = schema::<CleanReport<'static>>(SchemaSettings::draft2020_12().for_serialize()),
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn clean_document(&self, args: JsonObject) -> CallToolResult {
        answer(parse(args).and_then(clean))
    }
}

#[tool_handler(
    name = "doc-sanitize",
    version = "0.1.0",
    instructions = "Check and clean hidden data in local PDF, Word, Excel and PowerPoint files before they are shared, and verify whether a PDF redaction is real. Pass absolute file paths. Findings leave out the hidden text itself unless show_values is true; keep it off when you only need to know what is there."
)]
impl ServerHandler for Server {}

/// The arguments, refusing names the tool doesn't define.
fn parse<T: DeserializeOwned>(args: JsonObject) -> Result<T, Error> {
    serde_json::from_value(Value::Object(args)).map_err(|e| {
        Error::usage(
            format!("Can't read the arguments: {e}."),
            "See the tool's input schema.",
        )
    })
}

/// The report as the result, or the error object as an error result. Errors carry no structured
/// content, so clients don't check them against the output schema.
fn answer(result: Result<Value, Error>) -> CallToolResult {
    result.map_or_else(
        |e| CallToolResult::error(vec![ContentBlock::text(e.to_json())]),
        CallToolResult::structured,
    )
}

/// The files `paths` name, as the CLI expands them.
fn inputs(paths: &[String], recursive: bool) -> Result<Vec<Input>, Error> {
    if paths.is_empty() {
        return Err(Error::usage("No file given.", "Pass at least one path."));
    }
    if paths.len() > 100 {
        return Err(Error::usage(
            "Too many paths.",
            "Pass at most 100 paths per call.",
        ));
    }
    let paths: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    input::expand(&paths, recursive)
}

fn check(args: CheckArgs) -> Result<Value, Error> {
    let inputs = inputs(&args.paths, args.recursive)?;
    let options = Options {
        show_values: args.show_values,
        password: args.password,
    };
    let files: Vec<_> = inputs
        .iter()
        .map(|i| run::check_input(i, &options).0)
        .collect();
    Ok(serde_json::to_value(Report::check(&files)).expect("results are plain data"))
}

fn clean(args: CleanArgs) -> Result<Value, Error> {
    let inputs = inputs(&args.paths, args.recursive)?;
    let options = CleanOptions::new(
        Options {
            show_values: args.show_values,
            password: args.password,
        },
        &args.keep,
        args.output.map(PathBuf::from),
        args.out_dir.map(PathBuf::from),
        args.overwrite,
        &inputs,
    )?;
    let mut files: Vec<_> = inputs
        .iter()
        .map(|i| run::clean_input(i, &options).0)
        .collect();
    for output in files.iter_mut().filter_map(|f| f.output.as_mut()) {
        if let Ok(full) = path::absolute(Path::new(output)) {
            *output = full.to_string_lossy().into_owned();
        }
    }
    Ok(serde_json::to_value(CleanReport::new(&files)).expect("results are plain data"))
}

/// A tool's arguments, closed to names it doesn't define.
fn input_schema<T: JsonSchema>() -> Arc<JsonObject> {
    let mut schema = schema::<T>(SchemaSettings::draft2020_12());
    Arc::make_mut(&mut schema).insert("additionalProperties".into(), false.into());
    schema
}

/// A schema with an object at its root, as 2025-11-25 clients require, and without the Rust
/// number formats schemars adds (uint32, int64), which JSON Schema doesn't define.
fn schema<T: JsonSchema>(settings: SchemaSettings) -> Arc<JsonObject> {
    let no_format = |s: &mut Schema| drop(s.remove("format"));
    let schema = settings
        .with_transform(RecursiveTransform(no_format))
        .into_generator()
        .into_root_schema_for::<T>();
    let Value::Object(mut schema) = schema.to_value() else {
        unreachable!("a root schema is an object")
    };
    schema.remove("title");
    schema.remove("description");
    schema.insert("type".into(), "object".into());
    Arc::new(schema)
}

/// Serves until the client closes stdin. Calls run one at a time on this thread; each file is
/// read on its own worker.
pub fn serve() -> Result<(), Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::server)?;
    runtime.block_on(async {
        match Server.serve(rmcp::transport::stdio()).await {
            Ok(service) => service.waiting().await.map(drop).map_err(Error::server),
            // stdin closed before the handshake ended: the client left, which is no failure.
            Err(ServerInitializeError::ConnectionClosed(_)) => Ok(()),
            // stdout closed before the answer went out: the client left too.
            Err(ServerInitializeError::TransportError { error, .. })
                if error
                    .error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe) =>
            {
                Ok(())
            }
            Err(e) => Err(Error::server(e)),
        }
    })
}
