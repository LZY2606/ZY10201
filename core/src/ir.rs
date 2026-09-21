//! Reconciled intermediate representation (IR) of the typeshared type graph.
//!
//! After parsing and [`crate::reconcile::reconcile_aliases`] every type has a
//! stable identity: the pair `(source module, original name)` distinguishes
//! Rust declarations, while `renamed` is the wire/generated name produced by
//! serde attributes. This module projects the parsed data onto a flat,
//! deterministic graph of [`IrNode`]s connected by resolved [`IrEdge`]s, then
//! derives:
//!
//! * strongly connected components (SCCs), so mutual recursion is presented as
//!   a stable group rather than a generic topological error;
//! * topological dependency groups (levels), produced with a deterministic
//!   source-declaration tie-break instead of incidental map iteration order;
//! * [`IrDiagnostic`]s for ambiguous identities, wire-name collisions,
//!   rename-induced cycles and generic parameter shadowing.
//!
//! The graph is the single structure all language backends consume. Backend
//! specific syntax differences happen later, during the per-language lowering
//! implemented in [`crate::language`].
//!
//! # Complexity
//!
//! Construction, edge collection and Tarjan SCC are `O(V + E)`. The
//! Kahn-style level assignment and stable tie-breaks are
//! `O((V + E) + k log k)` where `k` is the number of simultaneously
//! ready SCCs. JSON serialization is `O(V + E)`; output size is the same.

use crate::rust_types::{
    RustConst, RustEnum, RustEnumVariant, RustField, RustStruct, RustType, SpecialRustType,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Kind of a reconciled type node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrNodeKind {
    /// A `struct` with named fields.
    Struct,
    /// A unit or algebraic `enum`.
    Enum,
    /// A `type` alias or a newtype/tuple struct lowered to an alias.
    Alias,
    /// A numeric `const`.
    Const,
}

impl IrNodeKind {
    fn as_str(self) -> &'static str {
        match self {
            IrNodeKind::Struct => "struct",
            IrNodeKind::Enum => "enum",
            IrNodeKind::Alias => "alias",
            IrNodeKind::Const => "const",
        }
    }
}

/// A generic parameter declared on a type, with its index in the declaration
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrGenericParam {
    /// Parameter name, e.g. the `T` in `struct Foo<T>`.
    pub name: String,
    /// Zero-based declaration index.
    pub index: usize,
}

/// A struct/enum field after parsing and skip filtering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrField {
    /// Original Rust field name.
    pub original: String,
    /// Serialized/generated field name after `rename_all`/`rename`.
    pub renamed: String,
    /// Whether `#[serde(rename = "...")]` overrode `rename_all`.
    pub serde_rename: bool,
    /// Canonical display form of the field's type, e.g. `Vec<Option<T>>`.
    pub ty: String,
}

impl IrField {
    fn from(field: &RustField) -> Self {
        Self {
            original: field.id.original.clone(),
            renamed: field.id.renamed.clone(),
            serde_rename: field.id.serde_rename,
            ty: field.ty.to_string(),
        }
    }
}

/// An enum variant in the reconciled graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrVariant {
    /// Original variant identifier.
    pub original: String,
    /// Serialized variant name.
    pub renamed: String,
    /// Whether an explicit `serde(rename)` was used.
    pub serde_rename: bool,
    /// Variant payload kind.
    pub payload: IrVariantPayload,
}

/// Payload shape of an enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrVariantPayload {
    /// No associated data.
    Unit,
    /// A single tuple/newtype payload with the given type.
    Tuple(String),
    /// An anonymous struct with the given fields.
    AnonymousStruct(Vec<IrField>),
}

/// A single node in the reconciled type graph.
#[derive(Debug, Clone)]
pub struct IrNode {
    /// Stable graph key: `crate` + `module path` + `original name`.
    pub key: String,
    /// Original Rust identifier.
    pub original: String,
    /// Serialized/generated identifier after serde renaming.
    pub renamed: String,
    /// Whether the top-level type was renamed with `serde(rename)`.
    pub serde_rename: bool,
    /// Crate the type belongs to.
    pub crate_name: String,
    /// Nested source module path (empty for a crate-root item).
    pub module: Vec<String>,
    /// Struct/enum/alias/const discriminant.
    pub kind: IrNodeKind,
    /// Declared generic parameters in declaration order.
    pub generic_params: Vec<IrGenericParam>,
    /// Fields (structs only).
    pub fields: Vec<IrField>,
    /// Variants (enums only).
    pub variants: Vec<IrVariant>,
    /// Aliased type display form (aliases only).
    pub aliased_type: Option<String>,
    /// Enum representation.
    pub enum_repr: Option<IrEnumRepr>,
    /// True if the enum directly references its own original name.
    pub is_recursive: bool,
    /// Source declaration order (module walk order) used as a stable
    /// tie-break; never derived from a hash map.
    pub source_order: usize,
}

/// Enum representation metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrEnumRepr {
    /// All variants are unit variants; serialized as a bare string.
    Unit,
    /// Internally tagged adjacently-tagged representation with the given
    /// `tag`/`content` JSON keys.
    Tagged {
        /// JSON key carrying the variant discriminant.
        tag: String,
        /// JSON key carrying the variant payload.
        content: String,
    },
}

impl IrNode {
    fn identity(&self) -> String {
        let mut path = self.module.clone();
        path.push(self.original.clone());
        path.join("::")
    }
}

/// A resolved dependency edge between two nodes of the graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct IrEdge {
    /// Key of the node that depends on another.
    pub from: String,
    /// Key of the referenced node.
    pub from_module: Vec<String>,
    /// Key of the referenced node.
    pub to: String,
    /// Module of the referenced declaration.
    pub to_module: Vec<String>,
    /// Original name of the referenced declaration.
    pub to_original: String,
    /// Serialized name of the referenced declaration.
    pub to_renamed: String,
    /// Where the edge originates.
    pub site: IrEdgeSite,
    /// True when the reference was expressed using the target's pre-rename
    /// name and reconcile rewrote it to its serde-renamed identity.
    pub via_rename_reconcile: bool,
}

/// Syntactic site a dependency edge was collected from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum IrEdgeSite {
    /// A struct field, carrying the field's serialized name.
    Field(String),
    /// A tuple enum variant, carrying the variant's serialized name.
    TupleVariant(String),
    /// The right-hand side of a type alias.
    AliasRhs,
    /// The type of a const.
    ConstType,
}

impl IrEdgeSite {
    fn as_label(&self) -> String {
        match self {
            IrEdgeSite::Field(name) => format!("field:{name}"),
            IrEdgeSite::TupleVariant(name) => format!("tuple_variant:{name}"),
            IrEdgeSite::AliasRhs => "alias_rhs".to_string(),
            IrEdgeSite::ConstType => "const_type".to_string(),
        }
    }
}
/// Kind of an identity/naming diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IrDiagnosticKind {
    /// Two declarations share an original name within the flattened
    /// single-file namespace but live in different modules.
    SameNameDifferentModule,
    /// Two declarations serialize under the same wire name.
    SerializedNameConflict,
    /// An SCC only exists because a serde rename rewired references.
    RenameCycle,
    /// A generic parameter name shadows a named type in scope, so it is
    /// treated as the parameter and can never resolve to that type.
    GenericShadowing,
}

impl IrDiagnosticKind {
    fn as_str(self) -> &'static str {
        match self {
            IrDiagnosticKind::SameNameDifferentModule => "same_name_different_module",
            IrDiagnosticKind::SerializedNameConflict => "serialized_name_conflict",
            IrDiagnosticKind::RenameCycle => "rename_cycle",
            IrDiagnosticKind::GenericShadowing => "generic_shadowing",
        }
    }
}

/// A deterministic diagnostic about identity resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrDiagnostic {
    /// Diagnostic kind.
    pub kind: IrDiagnosticKind,
    /// Stable, human-readable explanation.
    pub message: String,
    /// Involved node keys, sorted.
    pub nodes: Vec<String>,
}

/// A strongly connected component of nodes.
#[derive(Debug, Clone)]
pub struct IrScc {
    /// Member node keys in source-declaration order.
    pub members: Vec<String>,
    /// Internal edges (both endpoints in the SCC).
    pub edges: Vec<(String, String)>,
    /// True when SCC membership is induced by a serde rename.
    pub induced_by_rename: bool,
}

/// One topological dependency group: nodes/SCCs at the same depth.
#[derive(Debug, Clone)]
pub struct IrTopoGroup {
    /// Zero-based level; members only depend on groups with smaller levels.
    pub level: usize,
    /// Member node keys in the deterministic emission order.
    pub members: Vec<String>,
}

/// The reconciled type graph shared by every language generator.
#[derive(Debug, Clone)]
pub struct IrGraph {
    /// Nodes keyed by their stable identity.
    pub nodes: BTreeMap<String, IrNode>,
    /// Resolved dependency edges, sorted and de-duplicated.
    pub edges: Vec<IrEdge>,
    /// Strongly connected components with more than one node, plus any node
    /// with a self edge.
    pub sccs: Vec<IrScc>,
    /// Topological groups over the SCC condensation.
    pub topo_groups: Vec<IrTopoGroup>,
    /// Identity diagnostics, sorted for stable snapshots.
    pub diagnostics: Vec<IrDiagnostic>,
}

fn make_key(crate_name: &str, module: &[String], original: &str) -> String {
    let mut parts = vec![crate_name.to_string()];
    parts.extend(module.iter().cloned());
    parts.push(original.to_string());
    parts.join("::")
}

/// Reference that reached a user-declared type by its (post-reconcile) name.
#[derive(Debug, Clone)]
struct TypeRef {
    /// Type id as it appears after reconcile.
    name: String,
    /// True when the id matched the original name of a serde-renamed node.
    via_rename_reconcile: bool,
}

fn collect_type_refs(
    ty: &RustType,
    out: &mut Vec<TypeRef>,
    renamed_names: &HashSet<String>,
) {
    match ty {
        RustType::Simple { id } => out.push(TypeRef {
            name: id.clone(),
            via_rename_reconcile: renamed_names.contains(id),
        }),
        RustType::Generic { id, parameters } => {
            out.push(TypeRef {
                name: id.clone(),
                via_rename_reconcile: renamed_names.contains(id),
            });
            for parameter in parameters {
                collect_type_refs(parameter, out, renamed_names);
            }
        }
        RustType::Special(special) => match special {
            SpecialRustType::Vec(inner)
            | SpecialRustType::Array(inner, _)
            | SpecialRustType::Slice(inner)
            | SpecialRustType::Option(inner) => collect_type_refs(inner, out, renamed_names),
            SpecialRustType::HashMap(key, value) => {
                collect_type_refs(key, out, renamed_names);
                collect_type_refs(value, out, renamed_names);
            }
            _ => {}
        },
    }
}
impl IrGraph {
    /// Build the reconciled graph from post-reconcile parsed data, keyed by
    /// crate name exactly as consumed by
    /// [`crate::reconcile::reconcile_aliases`].
    pub fn from_reconciled(
        crate_parsed_data: &BTreeMap<crate::language::CrateName, crate::parser::ParsedData>,
    ) -> Self {
        let mut nodes: BTreeMap<String, IrNode> = BTreeMap::new();
        let mut source_order = 0usize;

        let push_node = |node: IrNode,
                             nodes: &mut BTreeMap<String, IrNode>,
                             source_order: &mut usize| {
            nodes.insert(node.key.clone(), node);
            *source_order += 1;
        };

        for (crate_name, data) in crate_parsed_data {
            let crate_name = crate_name.as_str();

            for item in &data.aliases {
                push_node(
                    IrNode {
                        key: make_key(crate_name, &item.module, &item.id.original),
                        original: item.id.original.clone(),
                        renamed: item.id.renamed.clone(),
                        serde_rename: item.id.serde_rename,
                        crate_name: crate_name.to_string(),
                        module: item.module.clone(),
                        kind: IrNodeKind::Alias,
                        generic_params: make_generic_params(&item.generic_types),
                        fields: Vec::new(),
                        variants: Vec::new(),
                        aliased_type: Some(item.r#type.to_string()),
                        enum_repr: None,
                        is_recursive: false,
                        source_order,
                    },
                    &mut nodes,
                    &mut source_order,
                );
            }

            for item in &data.structs {
                push_node(node_from_struct(crate_name, item, source_order), &mut nodes, &mut source_order);
            }

            for item in &data.enums {
                push_node(node_from_enum(crate_name, item, source_order), &mut nodes, &mut source_order);
            }

            for item in &data.consts {
                push_node(node_from_const(crate_name, item, source_order), &mut nodes, &mut source_order);
            }
        }

        // After reconcile, references to a serde-renamed type use its *renamed*
        // name, so an edge whose target name is a renamed wire name was
        // introduced by the reconcile pass.
        let renamed_names: HashSet<String> = nodes
            .values()
            .filter(|n| n.serde_rename)
            .map(|n| n.renamed.clone())
            .collect();

        let by_name = index_by_name(&nodes);
        let edges = collect_edges(&nodes, &by_name, &renamed_names, crate_parsed_data);

        let adjacency = build_adjacency(&nodes, &edges);
        let sccs = tarjan_sccs(&nodes, &adjacency, &edges);
        let topo_groups = condensation_levels(&nodes, &sccs, &adjacency);
        let diagnostics = collect_diagnostics(&nodes, &sccs);

        Self {
            nodes,
            edges,
            sccs,
            topo_groups,
            diagnostics,
        }
    }
}

fn make_generic_params(names: &[String]) -> Vec<IrGenericParam> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| IrGenericParam {
            name: name.clone(),
            index,
        })
        .collect()
}

fn node_from_struct(crate_name: &str, item: &RustStruct, source_order: usize) -> IrNode {
    IrNode {
        key: make_key(crate_name, &item.module, &item.id.original),
        original: item.id.original.clone(),
        renamed: item.id.renamed.clone(),
        serde_rename: item.id.serde_rename,
        crate_name: crate_name.to_string(),
        module: item.module.clone(),
        kind: IrNodeKind::Struct,
        generic_params: make_generic_params(&item.generic_types),
        fields: item.fields.iter().map(IrField::from).collect(),
        variants: Vec::new(),
        aliased_type: None,
        enum_repr: None,
        is_recursive: false,
        source_order,
    }
}

fn node_from_enum(crate_name: &str, item: &RustEnum, source_order: usize) -> IrNode {
    let shared = item.shared();
    let (enum_repr, variants) = match item {
        RustEnum::Unit(_) => (
            IrEnumRepr::Unit,
            shared
                .variants
                .iter()
                .map(variant_ir)
                .collect::<Vec<_>>(),
        ),
        RustEnum::Algebraic { tag_key, content_key, .. } => (
            IrEnumRepr::Tagged {
                tag: tag_key.clone(),
                content: content_key.clone(),
            },
            shared.variants.iter().map(variant_ir).collect::<Vec<_>>(),
        ),
    };

    IrNode {
        key: make_key(crate_name, &shared.module, &shared.id.original),
        original: shared.id.original.clone(),
        renamed: shared.id.renamed.clone(),
        serde_rename: shared.id.serde_rename,
        crate_name: crate_name.to_string(),
        module: shared.module.clone(),
        kind: IrNodeKind::Enum,
        generic_params: make_generic_params(&shared.generic_types),
        fields: Vec::new(),
        variants,
        aliased_type: None,
        enum_repr: Some(enum_repr),
        is_recursive: shared.is_recursive,
        source_order,
    }
}

fn variant_ir(variant: &RustEnumVariant) -> IrVariant {
    let shared = variant.shared();
    let payload = match variant {
        RustEnumVariant::Unit(_) => IrVariantPayload::Unit,
        RustEnumVariant::Tuple { ty, .. } => IrVariantPayload::Tuple(ty.to_string()),
        RustEnumVariant::AnonymousStruct { fields, .. } => {
            IrVariantPayload::AnonymousStruct(fields.iter().map(IrField::from).collect())
        }
    };
    IrVariant {
        original: shared.id.original.clone(),
        renamed: shared.id.renamed.clone(),
        serde_rename: shared.id.serde_rename,
        payload,
    }
}

fn node_from_const(crate_name: &str, item: &RustConst, source_order: usize) -> IrNode {
    IrNode {
        key: make_key(crate_name, &item.module, &item.id.original),
        original: item.id.original.clone(),
        renamed: item.id.renamed.clone(),
        serde_rename: item.id.serde_rename,
        crate_name: crate_name.to_string(),
        module: item.module.clone(),
        kind: IrNodeKind::Const,
        generic_params: Vec::new(),
        fields: Vec::new(),
        variants: Vec::new(),
        aliased_type: Some(item.r#type.to_string()),
        enum_repr: None,
        is_recursive: false,
        source_order,
    }
}

fn index_by_name(nodes: &BTreeMap<String, IrNode>) -> HashMap<String, Vec<&IrNode>> {
    let mut index: HashMap<String, Vec<&IrNode>> = HashMap::new();
    for node in nodes.values() {
        index.entry(node.renamed.clone()).or_default().push(node);
        if node.renamed != node.original {
            index.entry(node.original.clone()).or_default().push(node);
        }
    }
    for candidates in index.values_mut() {
        // Deterministic preference: same-crate resolution in reconcile, then
        // shallowest module, then first source declaration.
        candidates.sort_by(|a, b| {
            a.module
                .len()
                .cmp(&b.module.len())
                .then_with(|| a.source_order.cmp(&b.source_order))
                .then_with(|| a.key.cmp(&b.key))
        });
    }
    index
}
fn resolve_node<'a>(
    referrer: &IrNode,
    name: &str,
    by_name: &HashMap<String, Vec<&'a IrNode>>,
) -> Option<&'a IrNode> {
    let candidates = by_name.get(name)?;
    let same_crate: Vec<&IrNode> = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.crate_name == referrer.crate_name)
        .collect();
    match same_crate.len() {
        // Exactly one in-crate declaration owns the name.
        1 => Some(same_crate[0]),
        // Ambiguous within the crate: the serialized-name-conflict diagnostic
        // reports it, and no edge is fabricated.
        _ if !same_crate.is_empty() => None,
        _ => match candidates.len() {
            1 => Some(candidates[0]),
            _ => None,
        },
    }
}

fn push_edge(
    edges: &mut BTreeSet<IrEdge>,
    from: &IrNode,
    to: &IrNode,
    site: IrEdgeSite,
    via_rename_reconcile: bool,
) {
    // Self dependencies are retained so Tarjan reports self-recursive SCCs.
    edges.insert(IrEdge {
        from: from.key.clone(),
        from_module: from.module.clone(),
        to: to.key.clone(),
        to_module: to.module.clone(),
        to_original: to.original.clone(),
        to_renamed: to.renamed.clone(),
        site,
        via_rename_reconcile,
    });
}

fn resolve_and_push(
    edges: &mut BTreeSet<IrEdge>,
    from: &IrNode,
    refs: &[TypeRef],
    site: IrEdgeSite,
    by_name: &HashMap<String, Vec<&IrNode>>,
) {
    let generics: HashSet<&str> = from
        .generic_params
        .iter()
        .map(|p| p.name.as_str())
        .collect();

    for reference in refs {
        if generics.contains(reference.name.as_str()) {
            continue;
        }
        if let Some(target) = resolve_node(from, &reference.name, by_name) {
            push_edge(edges, from, target, site.clone(), reference.via_rename_reconcile);
        }
    }
}

fn collect_edges(
    nodes: &BTreeMap<String, IrNode>,
    by_name: &HashMap<String, Vec<&IrNode>>,
    renamed_names: &HashSet<String>,
    crate_parsed_data: &BTreeMap<crate::language::CrateName, crate::parser::ParsedData>,
) -> Vec<IrEdge> {
    let mut edges: BTreeSet<IrEdge> = BTreeSet::new();
    let mut refs: Vec<TypeRef> = Vec::new();

    for (crate_name, data) in crate_parsed_data {
        let crate_name = crate_name.as_str();

        for item in &data.aliases {
            let key = make_key(crate_name, &item.module, &item.id.original);
            let Some(from) = nodes.get(&key) else { continue };
            refs.clear();
            collect_type_refs(&item.r#type, &mut refs, renamed_names);
            resolve_and_push(&mut edges, from, &refs, IrEdgeSite::AliasRhs, by_name);
        }

        for item in &data.structs {
            let key = make_key(crate_name, &item.module, &item.id.original);
            let Some(from) = nodes.get(&key) else { continue };
            for field in &item.fields {
                refs.clear();
                collect_type_refs(&field.ty, &mut refs, renamed_names);
                let site = IrEdgeSite::Field(field.id.renamed.clone());
                resolve_and_push(&mut edges, from, &refs, site, by_name);
            }
        }

        for item in &data.enums {
            let shared = item.shared();
            let key = make_key(crate_name, &shared.module, &shared.id.original);
            let Some(from) = nodes.get(&key) else { continue };
            if let RustEnum::Algebraic { .. } = item {
                // Anonymous-struct enum variant fields lower into generated
                // helper structs; the existing emitter emits those helpers
                // inline and never declares a dependency edge for them, so the
                // graph intentionally mirrors that behavior.
                for variant in &shared.variants {
                    if let RustEnumVariant::Tuple { ty, .. } = variant {
                        refs.clear();
                        collect_type_refs(ty, &mut refs, renamed_names);
                        let site =
                            IrEdgeSite::TupleVariant(variant.shared().id.renamed.clone());
                        resolve_and_push(&mut edges, from, &refs, site, by_name);
                    }
                }
            }
        }

        for item in &data.consts {
            let key = make_key(crate_name, &item.module, &item.id.original);
            let Some(from) = nodes.get(&key) else { continue };
            refs.clear();
            collect_type_refs(&item.r#type, &mut refs, renamed_names);
            resolve_and_push(&mut edges, from, &refs, IrEdgeSite::ConstType, by_name);
        }
    }

    edges.into_iter().collect()
}

fn build_adjacency(nodes: &BTreeMap<String, IrNode>, edges: &[IrEdge]) -> Vec<Vec<usize>> {
    let key_index: HashMap<&str, usize> = nodes
        .keys()
        .enumerate()
        .map(|(index, key)| (key.as_str(), index))
        .collect();

    let mut adjacency = vec![Vec::new(); nodes.len()];
    for edge in edges {
        let from = key_index[edge.from.as_str()];
        let to = key_index[edge.to.as_str()];
        adjacency[from].push(to);
    }
    for neighbours in &mut adjacency {
        neighbours.sort_unstable();
        neighbours.dedup();
    }
    adjacency
}
/// Iterative Tarjan SCC (recursion depth is unbounded on user input).
fn tarjan_sccs(
    nodes: &BTreeMap<String, IrNode>,
    adjacency: &[Vec<usize>],
    edges: &[IrEdge],
) -> Vec<IrScc> {
    let node_keys: Vec<&String> = nodes.keys().collect();
    let n = node_keys.len();

    let mut index = 0usize;
    let mut indices = vec![usize::MAX; n];
    let mut lowlink = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut components: Vec<Vec<usize>> = Vec::new();

    for root in 0..n {
        if indices[root] != usize::MAX {
            continue;
        }
        // (node, next-neighbour-to-visit)
        let mut call_stack: Vec<(usize, usize)> = vec![(root, 0)];
        indices[root] = index;
        lowlink[root] = index;
        index += 1;
        stack.push(root);
        on_stack[root] = true;

        while let Some(&(v, next)) = call_stack.last() {
            if next < adjacency[v].len() {
                let w = adjacency[v][next];
                call_stack.last_mut().unwrap().1 += 1;
                if indices[w] == usize::MAX {
                    indices[w] = index;
                    lowlink[w] = index;
                    index += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    call_stack.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(indices[w]);
                }
            } else {
                if lowlink[v] == indices[v] {
                    let mut component = Vec::new();
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w] = false;
                        component.push(w);
                        if w == v {
                            break;
                        }
                    }
                    components.push(component);
                }
                call_stack.pop();
                if let Some(&(parent, _)) = call_stack.last() {
                    lowlink[parent] = lowlink[parent].min(lowlink[v]);
                }
            }
        }
    }

    let edge_set: HashSet<(&str, &str)> = edges
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();
    let rename_edge: HashSet<(&str, &str)> = edges
        .iter()
        .filter(|edge| edge.via_rename_reconcile)
        .map(|edge| (edge.from.as_str(), edge.to.as_str()))
        .collect();

    let mut sccs = Vec::new();
    for component in components {
        let is_cycle = component.len() > 1
            || component
                .first()
                .is_some_and(|&v| edge_set.contains(&(node_keys[v].as_str(), node_keys[v].as_str())));
        if !is_cycle {
            continue;
        }

        let mut member_keys: Vec<String> =
            component.iter().map(|&v| node_keys[v].clone()).collect();
        let member_set: HashSet<&str> =
            member_keys.iter().map(String::as_str).collect();

        let mut internal_edges: Vec<(String, String)> = edges
            .iter()
            .filter(|edge| {
                member_set.contains(edge.from.as_str()) && member_set.contains(edge.to.as_str())
            })
            .map(|edge| (edge.from.clone(), edge.to.clone()))
            .collect();
        internal_edges.sort();
        internal_edges.dedup();

        let induced_by_rename = internal_edges
            .iter()
            .any(|(from, to)| rename_edge.contains(&(from.as_str(), to.as_str())));

        // SCC members are always presented in source-declaration order so the
        // snapshot cannot be affected by the order Tarjan pops the stack.
        member_keys.sort_by(|a, b| {
            nodes[a]
                .source_order
                .cmp(&nodes[b].source_order)
                .then_with(|| a.cmp(b))
        });

        sccs.push(IrScc {
            members: member_keys,
            edges: internal_edges,
            induced_by_rename,
        });
    }

    sccs.sort_by(|a, b| {
        a.members
            .first()
            .and_then(|key| nodes.get(key))
            .map(|n| n.source_order)
            .unwrap_or(usize::MAX)
            .cmp(
                &b.members
                    .first()
                    .and_then(|key| nodes.get(key))
                    .map(|n| n.source_order)
                    .unwrap_or(usize::MAX),
            )
    });
    sccs
}

/// Kahn levels over the SCC condensation.
///
/// The tie-break is the minimum source declaration order of each SCC, so the
/// resulting groups are deterministic without ever sorting the generated
/// output globally by name.
fn condensation_levels(
    nodes: &BTreeMap<String, IrNode>,
    sccs: &[IrScc],
    adjacency: &[Vec<usize>],
) -> Vec<IrTopoGroup> {
    let node_keys: Vec<&String> = nodes.keys().collect();
    let key_index: HashMap<&str, usize> = node_keys
        .iter()
        .enumerate()
        .map(|(index, key)| (key.as_str(), index))
        .collect();

    // Map every node to its SCC index; singleton nodes get a synthetic SCC.
    let mut node_scc: Vec<usize> = vec![usize::MAX; nodes.len()];
    for (scc_index, scc) in sccs.iter().enumerate() {
        for member in &scc.members {
            node_scc[key_index[member.as_str()]] = scc_index;
        }
    }
    let mut singleton: Vec<Vec<String>> = Vec::new();
    for (node_index, key) in node_keys.iter().enumerate() {
        if node_scc[node_index] == usize::MAX {
            let scc_index = sccs.len() + singleton.len();
            node_scc[node_index] = scc_index;
            singleton.push(vec![(*key).clone()]);
        }
    }

    let all_scc_members: Vec<Vec<String>> = sccs
        .iter()
        .map(|scc| scc.members.clone())
        .chain(singleton)
        .collect();

    let count = all_scc_members.len();
    // `adjacency` points from a node to the nodes it depends on. A group is
    // ready once it has no dependencies left, so Kahn indegree counts incoming
    // dependency edges (the number of SCCs this SCC depends on).
    let mut scc_adjacency = vec![BTreeSet::new(); count];
    let mut indegree = vec![0usize; count];
    for (from_index, neighbours) in adjacency.iter().enumerate() {
        let from_scc = node_scc[from_index];
        for &to_index in neighbours {
            let to_scc = node_scc[to_index];
            // Edge from_scc -> to_scc means from_scc depends on to_scc.
            if from_scc != to_scc && scc_adjacency[from_scc].insert(to_scc) {
                indegree[from_scc] += 1;
            }
        }
    }

    let scc_source_order = |scc_index: usize| -> usize {
        all_scc_members[scc_index]
            .iter()
            .map(|key| nodes[key].source_order)
            .min()
            .unwrap_or(usize::MAX)
    };

    let mut ready: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (scc_index, &degree) in indegree.iter().enumerate() {
        if degree == 0 {
            ready.insert((scc_source_order(scc_index), scc_index));
        }
    }

    let mut groups: Vec<IrTopoGroup> = Vec::new();
    while !ready.is_empty() {
        let level = groups.len();
        let current: Vec<usize> = ready.iter().map(|&(_, index)| index).collect();
        ready.clear();

        let mut members = Vec::new();
        for scc_index in current {
            let mut scc_members = all_scc_members[scc_index].clone();
            scc_members.sort_by(|a, b| {
                nodes[a]
                    .source_order
                    .cmp(&nodes[b].source_order)
                    .then_with(|| a.cmp(b))
            });
            members.extend(scc_members);

            // `scc_index` was a dependency of every SCC that points at it.
            for dependent in 0..count {
                if scc_adjacency[dependent].contains(&scc_index) {
                    indegree[dependent] -= 1;
                    if indegree[dependent] == 0 {
                        ready.insert((scc_source_order(dependent), dependent));
                    }
                }
            }
        }

        // Members within a level are emitted in source declaration order.
        members.sort_by(|a, b| {
            nodes[a]
                .source_order
                .cmp(&nodes[b].source_order)
                .then_with(|| a.cmp(b))
        });

        groups.push(IrTopoGroup { level, members });
    }

    groups
}
fn collect_diagnostics(nodes: &BTreeMap<String, IrNode>, sccs: &[IrScc]) -> Vec<IrDiagnostic> {
    let mut diagnostics = Vec::new();

    // Same original name, distinct declaring modules/crates.
    let mut by_original: BTreeMap<String, Vec<&IrNode>> = BTreeMap::new();
    for node in nodes.values() {
        by_original.entry(node.original.clone()).or_default().push(node);
    }
    for (original, group) in &by_original {
        if group.len() > 1 {
            let distinct_modules: BTreeSet<String> =
                group.iter().map(|node| node.identity()).collect();
            if distinct_modules.len() > 1 {
                let mut member_nodes: Vec<String> = group.iter().map(|node| node.key.clone()).collect();
                member_nodes.sort();
                diagnostics.push(IrDiagnostic {
                    kind: IrDiagnosticKind::SameNameDifferentModule,
                    message: format!(
                        "type `{original}` is declared in multiple modules: {}; \
                         identity is resolved as crate + module + original name",
                        distinct_modules.iter().join(", ")
                    ),
                    nodes: member_nodes,
                });
            }
        }
    }

    // Same serialized name from different declarations.
    let mut by_renamed: BTreeMap<String, Vec<&IrNode>> = BTreeMap::new();
    for node in nodes.values() {
        by_renamed.entry(node.renamed.clone()).or_default().push(node);
    }
    for (renamed, group) in &by_renamed {
        let distinct: BTreeSet<&str> = group.iter().map(|node| node.key.as_str()).collect();
        if distinct.len() > 1 {
            let origins: Vec<String> = group
                .iter()
                .map(|node| node.identity())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let mut member_nodes: Vec<String> = group.iter().map(|node| node.key.clone()).collect();
            member_nodes.sort();
            diagnostics.push(IrDiagnostic {
                kind: IrDiagnosticKind::SerializedNameConflict,
                message: format!(
                    "serialized name `{renamed}` is produced by multiple declarations: {}; \
                     generated and wire identities collide",
                    origins.join(", ")
                ),
                nodes: member_nodes,
            });
        }
    }

    // Rename-induced SCCs.
    for scc in sccs.iter().filter(|scc| scc.induced_by_rename) {
        let mut members = scc.members.clone();
        members.sort();
        diagnostics.push(IrDiagnostic {
            kind: IrDiagnosticKind::RenameCycle,
            message: format!(
                "strongly connected group [{}] contains a reference rewritten by `serde(rename)`; \
                 the cycle is emitted as an SCC with backend-chosen forward declarations",
                members.join(", ")
            ),
            nodes: members,
        });
    }

    // Generic parameter shadowing a named type in the graph.
    let declared_names: HashSet<String> = nodes.values().map(|node| node.original.clone()).collect();
    for node in nodes.values() {
        let shadowed: Vec<String> = node
            .generic_params
            .iter()
            .map(|param| &param.name)
            .filter(|name| declared_names.contains(*name))
            .cloned()
            .collect();
        for name in shadowed {
            diagnostics.push(IrDiagnostic {
                kind: IrDiagnosticKind::GenericShadowing,
                message: format!(
                    "generic parameter `{name}` of `{}` shadows a named type; occurrences of \
                     `{name}` inside this item resolve to the parameter, not the shadowed type",
                    node.identity()
                ),
                nodes: vec![node.key.clone()],
            });
        }
    }

    diagnostics.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.nodes.cmp(&b.nodes))
            .then_with(|| a.message.cmp(&b.message))
    });
    diagnostics.dedup_by(|a, b| a.kind == b.kind && a.nodes == b.nodes);
    diagnostics
}

use itertools::Itertools;

/// Deterministic JSON value used to render the graph. Object key order is the
/// insertion order of the call that builds the object; every collection is
/// sorted by the builder.
#[derive(Debug, Clone)]
enum JsonValue {
    Bool(bool),
    Number(usize),
    String(String),
    Array(Vec<JsonValue>),
    /// Preserves insertion order.
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    fn string(value: impl Into<String>) -> Self {
        Self::String(value.into())
    }

    fn push(&mut self, key: impl Into<String>, value: impl Into<JsonValue>) {
        if let JsonValue::Object(entries) = self {
            entries.push((key.into(), value.into()));
        } else {
            panic!("JsonValue::push called on a non-object");
        }
    }

    fn render(&self, out: &mut String, indent: usize) {
        let pad = "  ".repeat(indent);
        match self {
            JsonValue::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            JsonValue::Number(value) => {
                use std::fmt::Write as _;
                let _ = write!(out, "{value}");
            }
            JsonValue::String(value) => render_string(value, out),
            JsonValue::Array(values) => {
                if values.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (index, value) in values.iter().enumerate() {
                    out.push_str(&"  ".repeat(indent + 1));
                    value.render(out, indent + 1);
                    if index + 1 < values.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push(']');
            }
            JsonValue::Object(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{\n");
                for (index, (key, value)) in entries.iter().enumerate() {
                    out.push_str(&"  ".repeat(indent + 1));
                    render_string(key, out);
                    out.push_str(": ");
                    value.render(out, indent + 1);
                    if index + 1 < entries.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push('}');
            }
        }
    }
}

impl From<bool> for JsonValue {
    fn from(value: bool) -> Self {
        JsonValue::Bool(value)
    }
}

impl From<usize> for JsonValue {
    fn from(value: usize) -> Self {
        JsonValue::Number(value)
    }
}

impl From<String> for JsonValue {
    fn from(value: String) -> Self {
        JsonValue::String(value)
    }
}

impl From<&str> for JsonValue {
    fn from(value: &str) -> Self {
        JsonValue::String(value.to_string())
    }
}

impl From<Vec<JsonValue>> for JsonValue {
    fn from(value: Vec<JsonValue>) -> Self {
        JsonValue::Array(value)
    }
}

fn render_string(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

fn strings(values: &[String]) -> JsonValue {
    JsonValue::Array(values.iter().map(|value| JsonValue::string(value.clone())).collect())
}

impl IrGraph {
    /// Serialize the graph to deterministic pretty JSON.
    ///
    /// The format is hand-written (the core crate has no serde dependency)
    /// and contains only graph data: no file paths, no hash-map iteration
    /// order and no toolchain/version strings.
    pub fn to_json_pretty(&self) -> String {
        let mut root = JsonValue::Object(Vec::new());

        root.push("nodes", self.nodes_json());
        root.push("edges", self.edges_json());
        root.push("sccs", self.sccs_json());
        root.push("topo_groups", self.topo_groups_json());
        root.push("diagnostics", self.diagnostics_json());

        let mut out = String::new();
        root.render(&mut out, 0);
        out.push('\n');
        out
    }

    fn nodes_json(&self) -> JsonValue {
        JsonValue::Array(
            self.nodes
                .values()
                .map(|node| {
                    let mut object = JsonValue::Object(Vec::new());
                    object.push("key", node.key.clone());
                    object.push("kind", node.kind.as_str());
                    object.push("original", node.original.clone());
                    object.push("renamed", node.renamed.clone());
                    object.push("serde_rename", node.serde_rename);
                    object.push("crate", node.crate_name.clone());
                    object.push("module", strings(&node.module));
                    object.push("generic_params", {
                        JsonValue::Array(
                            node.generic_params
                                .iter()
                                .map(|param| {
                                    let mut entry = JsonValue::Object(Vec::new());
                                    entry.push("name", param.name.clone());
                                    entry.push("index", param.index);
                                    entry
                                })
                                .collect(),
                        )
                    });
                    if let Some(ty) = &node.aliased_type {
                        object.push("aliased_type", ty.clone());
                    }
                    if let Some(repr) = &node.enum_repr {
                        let mut value = JsonValue::Object(Vec::new());
                        match repr {
                            IrEnumRepr::Unit => {
                                value.push("tagging", "unit");
                            }
                            IrEnumRepr::Tagged { tag, content } => {
                                value.push("tagging", "adjacently_tagged");
                                value.push("tag", tag.clone());
                                value.push("content", content.clone());
                            }
                        }
                        object.push("enum_repr", value);
                    }
                    if node.is_recursive {
                        object.push("is_recursive", true);
                    }
                    if !node.fields.is_empty() {
                        object.push("fields", fields_json(&node.fields));
                    }
                    if !node.variants.is_empty() {
                        object.push("variants", {
                            JsonValue::Array(
                                node.variants
                                    .iter()
                                    .map(|variant| {
                                        let mut value = JsonValue::Object(Vec::new());
                                        value.push("original", variant.original.clone());
                                        value.push("renamed", variant.renamed.clone());
                                        value.push("serde_rename", variant.serde_rename);
                                        match &variant.payload {
                                            IrVariantPayload::Unit => {
                                                value.push("payload", "unit");
                                            }
                                            IrVariantPayload::Tuple(ty) => {
                                                value.push("payload", "tuple");
                                                value.push("payload_ty", ty.clone());
                                            }
                                            IrVariantPayload::AnonymousStruct(fields) => {
                                                value.push("payload", "anonymous_struct");
                                                value.push("payload_fields", fields_json(fields));
                                            }
                                        }
                                        value
                                    })
                                    .collect(),
                            )
                        });
                    }
                    object
                })
                .collect(),
        )
    }

    fn edges_json(&self) -> JsonValue {
        JsonValue::Array(
            self.edges
                .iter()
                .map(|edge| {
                    let mut value = JsonValue::Object(Vec::new());
                    value.push("from", edge.from.clone());
                    value.push("to", edge.to.clone());
                    value.push("from_module", strings(&edge.from_module));
                    value.push("to_module", strings(&edge.to_module));
                    value.push("to_original", edge.to_original.clone());
                    value.push("to_renamed", edge.to_renamed.clone());
                    value.push("site", edge.site.as_label());
                    value.push("via_rename_reconcile", edge.via_rename_reconcile);
                    value
                })
                .collect(),
        )
    }

    fn sccs_json(&self) -> JsonValue {
        JsonValue::Array(
            self.sccs
                .iter()
                .map(|scc| {
                    let mut value = JsonValue::Object(Vec::new());
                    value.push("members", strings(&scc.members));
                    value.push(
                        "edges",
                        JsonValue::Array(
                            scc.edges
                                .iter()
                                .map(|(from, to)| {
                                    JsonValue::Array(vec![
                                        JsonValue::string(from.clone()),
                                        JsonValue::string(to.clone()),
                                    ])
                                })
                                .collect(),
                        ),
                    );
                    value.push("induced_by_rename", scc.induced_by_rename);
                    value
                })
                .collect(),
        )
    }

    fn topo_groups_json(&self) -> JsonValue {
        JsonValue::Array(
            self.topo_groups
                .iter()
                .map(|group| {
                    let mut value = JsonValue::Object(Vec::new());
                    value.push("level", group.level);
                    value.push("members", strings(&group.members));
                    value
                })
                .collect(),
        )
    }

    fn diagnostics_json(&self) -> JsonValue {
        JsonValue::Array(
            self.diagnostics
                .iter()
                .map(|diagnostic| {
                    let mut value = JsonValue::Object(Vec::new());
                    value.push("kind", diagnostic.kind.as_str());
                    value.push("message", diagnostic.message.clone());
                    value.push("nodes", strings(&diagnostic.nodes));
                    value
                })
                .collect(),
        )
    }
}

fn fields_json(fields: &[IrField]) -> JsonValue {
    JsonValue::Array(
        fields
            .iter()
            .map(|field| {
                let mut value = JsonValue::Object(Vec::new());
                value.push("original", field.original.clone());
                value.push("renamed", field.renamed.clone());
                value.push("serde_rename", field.serde_rename);
                value.push("ty", field.ty.clone());
                value
            })
            .collect(),
    )
}
