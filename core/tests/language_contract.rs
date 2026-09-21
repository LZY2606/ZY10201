//! Shared contract tests for the language generators.
//!
//! Every backend consumes the *same* reconciled type graph ([`IrGraph`]) built
//! from the *same* parsed data. These tests prove that:
//!
//! 1. every reconciled type is declared by every backend under its final
//!    (serde-renamed) identity;
//! 2. the emission order in every backend respects the graph's dependency
//!    edges: a dependency is never emitted after its dependent (members of a
//!    strongly connected component are exempt — backends break the cycle with
//!    language-specific forward declarations);
//! 3. genuine per-language lowering differences are recorded in a stable
//!    manifest snapshot, so a change in one backend cannot silently leak into
//!    the shared contract.

mod ir_support;

use ir_support::{graph_from, parse_fixture};
use std::collections::HashMap;
use typeshare_core::language::{Go, Kotlin, Language, Swift, TypeScript};

/// One fixture exercising structs, aliases, tagged enums, anonymous-struct
/// variants, a self-recursive enum and a mutually recursive pair.
const CONTRACT_SOURCE: &str = r#"
#[typeshare]
pub struct Leaf {
    pub label: String,
}

#[typeshare]
pub struct Branch {
    pub leaves: Vec<Leaf>,
}

#[typeshare]
pub struct Trunk {
    pub branch: Branch,
    pub leaf: Leaf,
}

#[typeshare]
pub type LeafList = Vec<Leaf>;

#[typeshare]
#[serde(tag = "type", content = "data")]
pub enum Shape {
    Circle(Leaf),
    Rectangle { width: u32, height: u32 },
}

#[typeshare]
#[serde(tag = "type", content = "data")]
pub enum Tree {
    Leaf(u32),
    Node(Box<Tree>),
}

#[typeshare]
pub struct Alpha {
    pub beta: Box<Beta>,
}

#[typeshare]
pub struct Beta {
    pub alpha: Box<Alpha>,
}
"#;

fn generate(mut language: impl Language, fixture: &ir_support::Fixture) -> String {
    let mut out = Vec::new();
    language
        .generate_types(
            &mut out,
            &HashMap::new(),
            ir_support::clone_parsed_data(&fixture.data),
        )
        .expect("generation failed");
    String::from_utf8(out).expect("utf8 output")
}

/// Extract the ordered top-level type declarations from generated code.
fn declared_types(language: &str, output: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in output.lines() {
        let line = line.trim_start();
        let name = match language {
            "typescript" => ["export interface ", "export type ", "export enum ", "export const "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
                .map(|rest| take_ident(rest)),
            "swift" => [
                "public struct ",
                "public indirect enum ",
                "public enum ",
                "public typealias ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(|rest| take_ident(rest)),
            "kotlin" => [
                "data class ",
                "enum class ",
                "sealed class ",
                "value class ",
                "typealias ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(|rest| take_ident(rest)),
            "go" => line
                .strip_prefix("type ")
                .map(|rest| take_ident(rest)),
            other => panic!("no extractor for {other}"),
        };
        if let Some(name) = name {
            if !name.is_empty() {
                names.push(name);
            }
        }
    }
    names
}

fn take_ident(rest: &str) -> String {
    rest.chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect()
}

/// The reconciled graph for the shared fixture, built once and shared by all
/// backend runs in this test.
fn contract_graph() -> (ir_support::Fixture, typeshare_core::ir::IrGraph) {
    let fixture = parse_fixture("contract_crate", CONTRACT_SOURCE);
    let graph = graph_from(std::slice::from_ref(&fixture));
    (fixture, graph)
}

#[test]
fn all_backends_consume_the_same_reconciled_graph() {
    let (fixture, graph) = contract_graph();

    let outputs: Vec<(&str, String)> = vec![
        ("typescript", generate(TypeScript::default(), &fixture)),
        ("swift", generate(Swift::default(), &fixture)),
        ("kotlin", generate(Kotlin::default(), &fixture)),
        ("go", generate(Go::default(), &fixture)),
    ];

    // 1. Every reconciled node is declared by every backend under its final
    //    serialized identity.
    for (language, output) in &outputs {
        let declared = declared_types(language, output);
        for node in graph.nodes.values() {
            assert!(
                declared.iter().any(|name| name == &node.renamed),
                "backend `{language}` did not declare reconciled type `{}` \
                 (key `{}`); declared: {declared:?}",
                node.renamed,
                node.key
            );
        }
    }

    // 2. Emission order respects dependency edges across SCC boundaries.
    //    Within an SCC the order is a per-backend choice (forward
    //    declarations, `indirect`, ...), so intra-SCC pairs are exempt.
    let scc_of = |key: &str| -> Option<usize> {
        graph
            .sccs
            .iter()
            .position(|scc| scc.members.iter().any(|member| member == key))
    };

    for (language, output) in &outputs {
        let declared = declared_types(language, output);
        let position = |name: &str| declared.iter().position(|n| n == name);
        for edge in &graph.edges {
            if edge.from == edge.to {
                continue;
            }
            let from_node = &graph.nodes[&edge.from];
            let to_node = &graph.nodes[&edge.to];
            if scc_of(&edge.from).is_some() && scc_of(&edge.from) == scc_of(&edge.to) {
                continue;
            }
            let (Some(dependent_pos), Some(dependency_pos)) =
                (position(&from_node.renamed), position(&to_node.renamed))
            else {
                panic!(
                    "backend `{language}` is missing a declaration for edge {} -> {}",
                    edge.from, edge.to
                );
            };
            assert!(
                dependency_pos < dependent_pos,
                "backend `{language}` emitted `{}` (dependency) after `{}` (dependent); \
                 declared order: {declared:?}",
                to_node.renamed,
                from_node.renamed
            );
        }
    }
}

#[test]
fn lowering_differences_manifest() {
    let (fixture, _graph) = contract_graph();

    let typescript = generate(TypeScript::default(), &fixture);
    let swift = generate(Swift::default(), &fixture);
    let kotlin = generate(Kotlin::default(), &fixture);
    let go = generate(Go::default(), &fixture);

    // Recorded lowering differences between backends consuming the same graph:
    // * TypeScript inlines anonymous-struct enum variants; Swift, Kotlin and
    //   Go emit `{Enum}{Variant}Inner` helper types.
    // * Swift marks self-recursive enums `indirect`; other backends have no
    //   equivalent keyword.
    // * Go lowers unit-less algebraic enums to a string type plus const
    //   blocks; Kotlin uses a sealed class; TypeScript a tagged union.
    let manifest = format!(
        "typescript declarations: {:?}\n\
         typescript emits ShapeRectangleInner helper: {}\n\
         swift declarations: {:?}\n\
         swift emits ShapeRectangleInner helper: {}\n\
         swift marks recursive enum indirect: {}\n\
         kotlin declarations: {:?}\n\
         kotlin emits ShapeRectangleInner helper: {}\n\
         go declarations: {:?}\n\
         go emits ShapeRectangleInner helper: {}\n",
        declared_types("typescript", &typescript),
        typescript.contains("ShapeRectangleInner"),
        declared_types("swift", &swift),
        swift.contains("ShapeRectangleInner"),
        swift.contains("public indirect enum Tree"),
        declared_types("kotlin", &kotlin),
        kotlin.contains("ShapeRectangleInner"),
        declared_types("go", &go),
        go.contains("ShapeRectangleInner"),
    );

    expect_test::expect![[r#"
        typescript declarations: ["Leaf", "LeafList", "Beta", "Alpha", "Branch", "Trunk", "Shape", "Tree"]
        typescript emits ShapeRectangleInner helper: false
        swift declarations: ["Leaf", "LeafList", "Beta", "Alpha", "Branch", "Trunk", "ShapeRectangleInner", "Shape", "Tree"]
        swift emits ShapeRectangleInner helper: true
        swift marks recursive enum indirect: true
        kotlin declarations: ["Leaf", "LeafList", "Beta", "Alpha", "Branch", "Trunk", "ShapeRectangleInner", "Shape", "Circle", "Rectangle", "Tree", "Leaf", "Node"]
        kotlin emits ShapeRectangleInner helper: true
        go declarations: ["Leaf", "LeafList", "Beta", "Alpha", "Branch", "Trunk", "ShapeRectangleInner", "ShapeTypes", "Shape", "TreeTypes", "Tree"]
        go emits ShapeRectangleInner helper: true
    "#]]
    .assert_eq(&manifest);
}
