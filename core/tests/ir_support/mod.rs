//! Shared support for the reconciled type-graph (IR) snapshot tests.
//!
//! Everything here is in-memory: no temporary files, no file-system walk and
//! no network or clock access. Inputs are inline Rust string fixtures, and
//! snapshot rendering is done with `expect-test`, whose failures print a
//! readable context diff.

#![allow(dead_code)]

use std::collections::BTreeMap;
use typeshare_core::context::{ParseContext, ParseFileContext};
use typeshare_core::ir::IrGraph;
use typeshare_core::language::CrateName;
use typeshare_core::parser::{self, ParsedData};
use typeshare_core::reconcile::reconcile_aliases;

/// Result of lowering one inline Rust fixture.
pub struct Fixture {
    /// The virtual crate the fixture was parsed as.
    pub crate_name: String,
    /// Post-reconcile parsed data.
    pub data: ParsedData,
}

/// Parse a single-crate inline Rust fixture, running the same reconcile step
/// the CLI performs before generation.
pub fn parse_fixture(crate_name: &str, source: &str) -> Fixture {
    parse_fixture_inner(crate_name, "fixture.rs", source)
        .unwrap_or_else(|err| panic!("failed to parse fixture in crate `{crate_name}`:\n{source}\nerror: {err}"))
}

/// Parse result for attribute tests: typeshare collects per-item failures into
/// `ParsedData::errors` instead of aborting the whole file.
pub struct ParseOutcome {
    pub parse_error: Option<String>,
    pub collected_errors: Vec<String>,
}

pub fn parse_outcome(crate_name: &str, source: &str) -> ParseOutcome {
    let context = typeshare_core::context::ParseContext::default();
    let file_context = typeshare_core::context::ParseFileContext {
        source_code: source.to_string(),
        crate_name: crate_name.into(),
        file_name: "fixture.rs".to_string(),
        file_path: "fixture.rs".into(),
    };
    match parser::parse(&context, file_context) {
        Err(error) => ParseOutcome {
            parse_error: Some(error.to_string()),
            collected_errors: Vec::new(),
        },
        Ok(Some(data)) => ParseOutcome {
            parse_error: None,
            collected_errors: data.errors.iter().map(|e| e.error.clone()).collect(),
        },
        Ok(None) => ParseOutcome {
            parse_error: None,
            collected_errors: Vec::new(),
        },
    }
}

fn parse_fixture_inner(
    crate_name: &str,
    file_name: &str,
    source: &str,
) -> Result<Fixture, typeshare_core::error::ParseErrorWithSpan> {
    let context = ParseContext::default();
    let file_context = ParseFileContext {
        source_code: source.to_string(),
        crate_name: crate_name.into(),
        file_name: file_name.to_string(),
        // A fixed relative path keeps parse error output stable and free of
        // machine-specific absolute paths.
        file_path: "fixture.rs".into(),
    };

    let data = parser::parse(&context, file_context)?
        .expect("fixture contains a #[typeshare] item");

    let key: CrateName = crate_name.into();
    let mut map = BTreeMap::from([(key, data)]);
    reconcile_aliases(&mut map);
    let key: CrateName = crate_name.into();
    let data = map.remove(&key).unwrap();

    Ok(Fixture {
        crate_name: crate_name.to_string(),
        data,
    })
}

/// Parse several crates' inline fixtures and reconcile them together, exactly
/// like a multi-crate CLI invocation.
pub fn parse_fixtures<'a>(fixtures: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Fixture> {
    let mut map: BTreeMap<CrateName, ParsedData> = BTreeMap::new();
    for (crate_name, source) in fixtures {
        let context = ParseContext::default();
        let file_context = ParseFileContext {
            source_code: source.to_string(),
            crate_name: crate_name.into(),
            file_name: "fixture.rs".to_string(),
            file_path: "fixture.rs".into(),
        };
        let data = parser::parse(&context, file_context)
            .unwrap_or_else(|err| panic!("parse error in crate `{crate_name}`: {err}"))
            .unwrap_or_else(|| panic!("no typeshare items in crate `{crate_name}`"));
        map.insert(crate_name.into(), data);
    }

    reconcile_aliases(&mut map);
    map.into_iter()
        .map(|(crate_name, data)| Fixture {
            crate_name: crate_name.to_string(),
            data,
        })
        .collect()
}

/// Build the reconciled graph from one or more parsed fixtures.
pub fn graph_from(fixtures: &[Fixture]) -> IrGraph {
    let map: BTreeMap<CrateName, ParsedData> = fixtures
        .iter()
        .map(|fixture| {
            let key: CrateName = fixture.crate_name.as_str().into();
            (key, clone_parsed_data(&fixture.data))
        })
        .collect();
    IrGraph::from_reconciled(&map)
}

/// `ParsedData` has no `Clone`; rebuild an equivalent value for reuse across
/// graph construction and language generation.
pub fn clone_parsed_data(data: &ParsedData) -> ParsedData {
    let mut clone = ParsedData::new(
        data.crate_name.clone(),
        data.file_name.clone(),
        data.multi_file,
    );
    clone.structs = data.structs.clone();
    clone.enums = data.enums.clone();
    clone.aliases = data.aliases.clone();
    clone.consts = data.consts.clone();
    clone.import_types = data.import_types.clone();
    clone.type_names = data.type_names.clone();
    clone.errors = data
        .errors
        .iter()
        .map(|error| typeshare_core::parser::ErrorInfo {
            file_name: error.file_name.clone(),
            error: error.error.clone(),
        })
        .collect();
    clone
}

