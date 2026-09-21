//! Snapshot tests for the reconciled intermediate representation.
//!
//! Each test parses an inline Rust fixture and snapshots the deterministic
//! JSON graph: parsed names, source modules, serialized names, generic
//! parameters, dependency edges and the final topological groups.
//!
//! Snapshots intentionally contain no file paths (other than the fixed
//! `fixture.rs` marker used for parse-error tests), no hash-map order and no
//! compiler/version data.

mod ir_support;

use ir_support::{graph_from, parse_fixture};
use typeshare_core::ir::IrGraph;

fn render(graph: &IrGraph) -> String {
    graph.to_json_pretty()
}

#[test]
fn rename_all_and_field_rename() {
    let fixture = parse_fixture(
        "rename_crate",
        r#"
#[typeshare]
#[serde(rename_all = "camelCase")]
pub struct PersonRecord {
    pub first_name: String,
    pub last_name: String,
    #[serde(rename = "EMAIL")]
    pub email_address: String,
    pub age_years: u32,
}
"#,
    );
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "rename_crate::PersonRecord",
              "kind": "struct",
              "original": "PersonRecord",
              "renamed": "PersonRecord",
              "serde_rename": false,
              "crate": "rename_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "first_name",
                  "renamed": "firstName",
                  "serde_rename": false,
                  "ty": "String"
                },
                {
                  "original": "last_name",
                  "renamed": "lastName",
                  "serde_rename": false,
                  "ty": "String"
                },
                {
                  "original": "email_address",
                  "renamed": "EMAIL",
                  "serde_rename": true,
                  "ty": "String"
                },
                {
                  "original": "age_years",
                  "renamed": "ageYears",
                  "serde_rename": false,
                  "ty": "u32"
                }
              ]
            }
          ],
          "edges": [],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "rename_crate::PersonRecord"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]].assert_eq(&render(&graph_from(std::slice::from_ref(&fixture))));
}

#[test]
fn skipped_fields_are_absent() {
    let fixture = parse_fixture(
        "skip_crate",
        r#"
#[typeshare]
#[serde(rename_all = "camelCase")]
pub struct VisibleRecord {
    pub visible_field: String,
    #[serde(skip)]
    pub hidden_field: String,
    #[typeshare(skip)]
    pub also_hidden: u32,
}
"#,
    );
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "skip_crate::VisibleRecord",
              "kind": "struct",
              "original": "VisibleRecord",
              "renamed": "VisibleRecord",
              "serde_rename": false,
              "crate": "skip_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "visible_field",
                  "renamed": "visibleField",
                  "serde_rename": false,
                  "ty": "String"
                }
              ]
            }
          ],
          "edges": [],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "skip_crate::VisibleRecord"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]].assert_eq(&render(&graph_from(std::slice::from_ref(&fixture))));
}

#[test]
fn serde_flatten_is_rejected_with_diagnostic_context() {
    let outcome = ir_support::parse_outcome(
        "flatten_crate",
        r#"
#[typeshare]
pub struct FlattenHolder {
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, String>,
}
"#,
    );
    expect_test::expect![[
        "The serde flatten attribute is not currently supported, on line 4 and column 4"
    ]]
    .assert_eq(
        outcome
            .collected_errors
            .first()
            .or(outcome.parse_error.as_ref())
            .expect("flatten must produce a diagnostic"),
    );
}

#[test]
fn adjacently_tagged_enum() {
    let fixture = parse_fixture(
        "tagged_crate",
        r#"
#[typeshare]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    Created { id: u32, actor_name: String },
    Updated(u32),
    Closed,
}
"#,
    );
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "tagged_crate::Event",
              "kind": "enum",
              "original": "Event",
              "renamed": "Event",
              "serde_rename": false,
              "crate": "tagged_crate",
              "module": [],
              "generic_params": [],
              "enum_repr": {
                "tagging": "adjacently_tagged",
                "tag": "type",
                "content": "data"
              },
              "variants": [
                {
                  "original": "Created",
                  "renamed": "created",
                  "serde_rename": false,
                  "payload": "anonymous_struct",
                  "payload_fields": [
                    {
                      "original": "id",
                      "renamed": "id",
                      "serde_rename": false,
                      "ty": "u32"
                    },
                    {
                      "original": "actor_name",
                      "renamed": "actor_name",
                      "serde_rename": false,
                      "ty": "String"
                    }
                  ]
                },
                {
                  "original": "Updated",
                  "renamed": "updated",
                  "serde_rename": false,
                  "payload": "tuple",
                  "payload_ty": "u32"
                },
                {
                  "original": "Closed",
                  "renamed": "closed",
                  "serde_rename": false,
                  "payload": "unit"
                }
              ]
            }
          ],
          "edges": [],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "tagged_crate::Event"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]].assert_eq(&render(&graph_from(std::slice::from_ref(&fixture))));
}

#[test]
fn recursive_type_is_marked_and_self_edge() {
    let fixture = parse_fixture(
        "rec_crate",
        r#"
#[typeshare]
#[serde(tag = "type", content = "data")]
pub enum Tree {
    Leaf(u32),
    Node(Box<Tree>),
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "rec_crate::Tree",
              "kind": "enum",
              "original": "Tree",
              "renamed": "Tree",
              "serde_rename": false,
              "crate": "rec_crate",
              "module": [],
              "generic_params": [],
              "enum_repr": {
                "tagging": "adjacently_tagged",
                "tag": "type",
                "content": "data"
              },
              "is_recursive": true,
              "variants": [
                {
                  "original": "Leaf",
                  "renamed": "Leaf",
                  "serde_rename": false,
                  "payload": "tuple",
                  "payload_ty": "u32"
                },
                {
                  "original": "Node",
                  "renamed": "Node",
                  "serde_rename": false,
                  "payload": "tuple",
                  "payload_ty": "Tree"
                }
              ]
            }
          ],
          "edges": [
            {
              "from": "rec_crate::Tree",
              "to": "rec_crate::Tree",
              "from_module": [],
              "to_module": [],
              "to_original": "Tree",
              "to_renamed": "Tree",
              "site": "tuple_variant:Node",
              "via_rename_reconcile": false
            }
          ],
          "sccs": [
            {
              "members": [
                "rec_crate::Tree"
              ],
              "edges": [
                [
                  "rec_crate::Tree",
                  "rec_crate::Tree"
                ]
              ],
              "induced_by_rename": false
            }
          ],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "rec_crate::Tree"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn mutual_recursion_is_a_stable_scc_not_a_toposort_error() {
    let fixture = parse_fixture(
        "scc_crate",
        r#"
#[typeshare]
pub struct Parent {
    pub children: Vec<Child>,
    pub name: String,
}

#[typeshare]
pub struct Child {
    pub parent: Box<Parent>,
    pub kind: u8,
}

#[typeshare]
pub struct Root {
    pub first: Parent,
    pub note: String,
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "scc_crate::Child",
              "kind": "struct",
              "original": "Child",
              "renamed": "Child",
              "serde_rename": false,
              "crate": "scc_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "parent",
                  "renamed": "parent",
                  "serde_rename": false,
                  "ty": "Parent"
                },
                {
                  "original": "kind",
                  "renamed": "kind",
                  "serde_rename": false,
                  "ty": "u8"
                }
              ]
            },
            {
              "key": "scc_crate::Parent",
              "kind": "struct",
              "original": "Parent",
              "renamed": "Parent",
              "serde_rename": false,
              "crate": "scc_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "children",
                  "renamed": "children",
                  "serde_rename": false,
                  "ty": "Vec<Child>"
                },
                {
                  "original": "name",
                  "renamed": "name",
                  "serde_rename": false,
                  "ty": "String"
                }
              ]
            },
            {
              "key": "scc_crate::Root",
              "kind": "struct",
              "original": "Root",
              "renamed": "Root",
              "serde_rename": false,
              "crate": "scc_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "first",
                  "renamed": "first",
                  "serde_rename": false,
                  "ty": "Parent"
                },
                {
                  "original": "note",
                  "renamed": "note",
                  "serde_rename": false,
                  "ty": "String"
                }
              ]
            }
          ],
          "edges": [
            {
              "from": "scc_crate::Child",
              "to": "scc_crate::Parent",
              "from_module": [],
              "to_module": [],
              "to_original": "Parent",
              "to_renamed": "Parent",
              "site": "field:parent",
              "via_rename_reconcile": false
            },
            {
              "from": "scc_crate::Parent",
              "to": "scc_crate::Child",
              "from_module": [],
              "to_module": [],
              "to_original": "Child",
              "to_renamed": "Child",
              "site": "field:children",
              "via_rename_reconcile": false
            },
            {
              "from": "scc_crate::Root",
              "to": "scc_crate::Parent",
              "from_module": [],
              "to_module": [],
              "to_original": "Parent",
              "to_renamed": "Parent",
              "site": "field:first",
              "via_rename_reconcile": false
            }
          ],
          "sccs": [
            {
              "members": [
                "scc_crate::Child",
                "scc_crate::Parent"
              ],
              "edges": [
                [
                  "scc_crate::Child",
                  "scc_crate::Parent"
                ],
                [
                  "scc_crate::Parent",
                  "scc_crate::Child"
                ]
              ],
              "induced_by_rename": false
            }
          ],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "scc_crate::Child",
                "scc_crate::Parent"
              ]
            },
            {
              "level": 1,
              "members": [
                "scc_crate::Root"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn generic_alias_and_generic_instantiation_edges() {
    let fixture = parse_fixture(
        "generic_crate",
        r#"
#[typeshare]
pub struct Inner {
    pub value: u32,
}

#[typeshare]
pub type Wrapper<T> = Vec<T>;

#[typeshare]
pub struct Holder {
    pub a: Wrapper<Inner>,
    pub b: Wrapper<u32>,
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "generic_crate::Holder",
              "kind": "struct",
              "original": "Holder",
              "renamed": "Holder",
              "serde_rename": false,
              "crate": "generic_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "a",
                  "renamed": "a",
                  "serde_rename": false,
                  "ty": "Wrapper<Inner>"
                },
                {
                  "original": "b",
                  "renamed": "b",
                  "serde_rename": false,
                  "ty": "Wrapper<u32>"
                }
              ]
            },
            {
              "key": "generic_crate::Inner",
              "kind": "struct",
              "original": "Inner",
              "renamed": "Inner",
              "serde_rename": false,
              "crate": "generic_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "value",
                  "renamed": "value",
                  "serde_rename": false,
                  "ty": "u32"
                }
              ]
            },
            {
              "key": "generic_crate::Wrapper",
              "kind": "alias",
              "original": "Wrapper",
              "renamed": "Wrapper",
              "serde_rename": false,
              "crate": "generic_crate",
              "module": [],
              "generic_params": [
                {
                  "name": "T",
                  "index": 0
                }
              ],
              "aliased_type": "Vec<T>"
            }
          ],
          "edges": [
            {
              "from": "generic_crate::Holder",
              "to": "generic_crate::Inner",
              "from_module": [],
              "to_module": [],
              "to_original": "Inner",
              "to_renamed": "Inner",
              "site": "field:a",
              "via_rename_reconcile": false
            },
            {
              "from": "generic_crate::Holder",
              "to": "generic_crate::Wrapper",
              "from_module": [],
              "to_module": [],
              "to_original": "Wrapper",
              "to_renamed": "Wrapper",
              "site": "field:a",
              "via_rename_reconcile": false
            },
            {
              "from": "generic_crate::Holder",
              "to": "generic_crate::Wrapper",
              "from_module": [],
              "to_module": [],
              "to_original": "Wrapper",
              "to_renamed": "Wrapper",
              "site": "field:b",
              "via_rename_reconcile": false
            }
          ],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "generic_crate::Wrapper",
                "generic_crate::Inner"
              ]
            },
            {
              "level": 1,
              "members": [
                "generic_crate::Holder"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn same_name_different_modules_gets_distinct_identities() {
    let fixture = parse_fixture(
        "module_crate",
        r#"
mod billing {
    #[typeshare]
    pub struct Account {
        pub balance: u32,
    }
}

mod identity {
    #[typeshare]
    pub struct Account {
        pub display_name: String,
    }
}

#[typeshare]
pub struct Registry {
    pub billing: billing::Account,
    pub identity: identity::Account,
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "module_crate::Registry",
              "kind": "struct",
              "original": "Registry",
              "renamed": "Registry",
              "serde_rename": false,
              "crate": "module_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "billing",
                  "renamed": "billing",
                  "serde_rename": false,
                  "ty": "Account"
                },
                {
                  "original": "identity",
                  "renamed": "identity",
                  "serde_rename": false,
                  "ty": "Account"
                }
              ]
            },
            {
              "key": "module_crate::billing::Account",
              "kind": "struct",
              "original": "Account",
              "renamed": "Account",
              "serde_rename": false,
              "crate": "module_crate",
              "module": [
                "billing"
              ],
              "generic_params": [],
              "fields": [
                {
                  "original": "balance",
                  "renamed": "balance",
                  "serde_rename": false,
                  "ty": "u32"
                }
              ]
            },
            {
              "key": "module_crate::identity::Account",
              "kind": "struct",
              "original": "Account",
              "renamed": "Account",
              "serde_rename": false,
              "crate": "module_crate",
              "module": [
                "identity"
              ],
              "generic_params": [],
              "fields": [
                {
                  "original": "display_name",
                  "renamed": "display_name",
                  "serde_rename": false,
                  "ty": "String"
                }
              ]
            }
          ],
          "edges": [],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "module_crate::billing::Account",
                "module_crate::identity::Account",
                "module_crate::Registry"
              ]
            }
          ],
          "diagnostics": [
            {
              "kind": "same_name_different_module",
              "message": "type `Account` is declared in multiple modules: billing::Account, identity::Account; identity is resolved as crate + module + original name",
              "nodes": [
                "module_crate::billing::Account",
                "module_crate::identity::Account"
              ]
            },
            {
              "kind": "serialized_name_conflict",
              "message": "serialized name `Account` is produced by multiple declarations: billing::Account, identity::Account; generated and wire identities collide",
              "nodes": [
                "module_crate::billing::Account",
                "module_crate::identity::Account"
              ]
            }
          ]
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn serialized_name_conflict_is_diagnosed() {
    let fixture = parse_fixture(
        "wire_crate",
        r#"
#[typeshare]
#[serde(rename = "SharedWireName")]
pub struct Alpha {
    pub value: u32,
}

#[typeshare]
#[serde(rename = "SharedWireName")]
pub struct Beta {
    pub other: String,
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "wire_crate::Alpha",
              "kind": "struct",
              "original": "Alpha",
              "renamed": "SharedWireName",
              "serde_rename": true,
              "crate": "wire_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "value",
                  "renamed": "value",
                  "serde_rename": false,
                  "ty": "u32"
                }
              ]
            },
            {
              "key": "wire_crate::Beta",
              "kind": "struct",
              "original": "Beta",
              "renamed": "SharedWireName",
              "serde_rename": true,
              "crate": "wire_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "other",
                  "renamed": "other",
                  "serde_rename": false,
                  "ty": "String"
                }
              ]
            }
          ],
          "edges": [],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "wire_crate::Alpha",
                "wire_crate::Beta"
              ]
            }
          ],
          "diagnostics": [
            {
              "kind": "serialized_name_conflict",
              "message": "serialized name `SharedWireName` is produced by multiple declarations: Alpha, Beta; generated and wire identities collide",
              "nodes": [
                "wire_crate::Alpha",
                "wire_crate::Beta"
              ]
            }
          ]
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn rename_reconcile_and_rename_induced_cycle() {
    let fixture = parse_fixture(
        "rename_cycle_crate",
        r#"
#[typeshare]
#[serde(rename = "WireLeft")]
pub struct OldLeftName {
    pub points_to: OldRightName,
}

#[typeshare]
#[serde(rename = "WireRight")]
pub struct OldRightName {
    pub points_back: OldLeftName,
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "rename_cycle_crate::OldLeftName",
              "kind": "struct",
              "original": "OldLeftName",
              "renamed": "WireLeft",
              "serde_rename": true,
              "crate": "rename_cycle_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "points_to",
                  "renamed": "points_to",
                  "serde_rename": false,
                  "ty": "WireRight"
                }
              ]
            },
            {
              "key": "rename_cycle_crate::OldRightName",
              "kind": "struct",
              "original": "OldRightName",
              "renamed": "WireRight",
              "serde_rename": true,
              "crate": "rename_cycle_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "points_back",
                  "renamed": "points_back",
                  "serde_rename": false,
                  "ty": "WireLeft"
                }
              ]
            }
          ],
          "edges": [
            {
              "from": "rename_cycle_crate::OldLeftName",
              "to": "rename_cycle_crate::OldRightName",
              "from_module": [],
              "to_module": [],
              "to_original": "OldRightName",
              "to_renamed": "WireRight",
              "site": "field:points_to",
              "via_rename_reconcile": true
            },
            {
              "from": "rename_cycle_crate::OldRightName",
              "to": "rename_cycle_crate::OldLeftName",
              "from_module": [],
              "to_module": [],
              "to_original": "OldLeftName",
              "to_renamed": "WireLeft",
              "site": "field:points_back",
              "via_rename_reconcile": true
            }
          ],
          "sccs": [
            {
              "members": [
                "rename_cycle_crate::OldLeftName",
                "rename_cycle_crate::OldRightName"
              ],
              "edges": [
                [
                  "rename_cycle_crate::OldLeftName",
                  "rename_cycle_crate::OldRightName"
                ],
                [
                  "rename_cycle_crate::OldRightName",
                  "rename_cycle_crate::OldLeftName"
                ]
              ],
              "induced_by_rename": true
            }
          ],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "rename_cycle_crate::OldLeftName",
                "rename_cycle_crate::OldRightName"
              ]
            }
          ],
          "diagnostics": [
            {
              "kind": "rename_cycle",
              "message": "strongly connected group [rename_cycle_crate::OldLeftName, rename_cycle_crate::OldRightName] contains a reference rewritten by `serde(rename)`; the cycle is emitted as an SCC with backend-chosen forward declarations",
              "nodes": [
                "rename_cycle_crate::OldLeftName",
                "rename_cycle_crate::OldRightName"
              ]
            }
          ]
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn generic_parameter_shadowing_is_diagnosed() {
    let fixture = parse_fixture(
        "shadow_crate",
        r#"
#[typeshare]
pub struct Thing {
    pub value: u32,
}

#[typeshare]
pub struct BoxOf<Thing> {
    pub item: Thing,
}

#[typeshare]
pub struct PlainHolder<Item> {
    pub item: Item,
    pub thing: Thing,
}
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "shadow_crate::BoxOf",
              "kind": "struct",
              "original": "BoxOf",
              "renamed": "BoxOf",
              "serde_rename": false,
              "crate": "shadow_crate",
              "module": [],
              "generic_params": [
                {
                  "name": "Thing",
                  "index": 0
                }
              ],
              "fields": [
                {
                  "original": "item",
                  "renamed": "item",
                  "serde_rename": false,
                  "ty": "Thing"
                }
              ]
            },
            {
              "key": "shadow_crate::PlainHolder",
              "kind": "struct",
              "original": "PlainHolder",
              "renamed": "PlainHolder",
              "serde_rename": false,
              "crate": "shadow_crate",
              "module": [],
              "generic_params": [
                {
                  "name": "Item",
                  "index": 0
                }
              ],
              "fields": [
                {
                  "original": "item",
                  "renamed": "item",
                  "serde_rename": false,
                  "ty": "Item"
                },
                {
                  "original": "thing",
                  "renamed": "thing",
                  "serde_rename": false,
                  "ty": "Thing"
                }
              ]
            },
            {
              "key": "shadow_crate::Thing",
              "kind": "struct",
              "original": "Thing",
              "renamed": "Thing",
              "serde_rename": false,
              "crate": "shadow_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "value",
                  "renamed": "value",
                  "serde_rename": false,
                  "ty": "u32"
                }
              ]
            }
          ],
          "edges": [
            {
              "from": "shadow_crate::PlainHolder",
              "to": "shadow_crate::Thing",
              "from_module": [],
              "to_module": [],
              "to_original": "Thing",
              "to_renamed": "Thing",
              "site": "field:thing",
              "via_rename_reconcile": false
            }
          ],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "shadow_crate::BoxOf",
                "shadow_crate::Thing"
              ]
            },
            {
              "level": 1,
              "members": [
                "shadow_crate::PlainHolder"
              ]
            }
          ],
          "diagnostics": [
            {
              "kind": "generic_shadowing",
              "message": "generic parameter `Thing` of `BoxOf` shadows a named type; occurrences of `Thing` inside this item resolve to the parameter, not the shadowed type",
              "nodes": [
                "shadow_crate::BoxOf"
              ]
            }
          ]
        }
    "#]]
    .assert_eq(&render(&graph));
}

#[test]
fn dependency_edges_drive_topological_groups() {
    let fixture = parse_fixture(
        "topo_crate",
        r#"
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
"#,
    );
    let graph = graph_from(std::slice::from_ref(&fixture));
    expect_test::expect![[r#"
        {
          "nodes": [
            {
              "key": "topo_crate::Branch",
              "kind": "struct",
              "original": "Branch",
              "renamed": "Branch",
              "serde_rename": false,
              "crate": "topo_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "leaves",
                  "renamed": "leaves",
                  "serde_rename": false,
                  "ty": "Vec<Leaf>"
                }
              ]
            },
            {
              "key": "topo_crate::Leaf",
              "kind": "struct",
              "original": "Leaf",
              "renamed": "Leaf",
              "serde_rename": false,
              "crate": "topo_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "label",
                  "renamed": "label",
                  "serde_rename": false,
                  "ty": "String"
                }
              ]
            },
            {
              "key": "topo_crate::Trunk",
              "kind": "struct",
              "original": "Trunk",
              "renamed": "Trunk",
              "serde_rename": false,
              "crate": "topo_crate",
              "module": [],
              "generic_params": [],
              "fields": [
                {
                  "original": "branch",
                  "renamed": "branch",
                  "serde_rename": false,
                  "ty": "Branch"
                },
                {
                  "original": "leaf",
                  "renamed": "leaf",
                  "serde_rename": false,
                  "ty": "Leaf"
                }
              ]
            }
          ],
          "edges": [
            {
              "from": "topo_crate::Branch",
              "to": "topo_crate::Leaf",
              "from_module": [],
              "to_module": [],
              "to_original": "Leaf",
              "to_renamed": "Leaf",
              "site": "field:leaves",
              "via_rename_reconcile": false
            },
            {
              "from": "topo_crate::Trunk",
              "to": "topo_crate::Branch",
              "from_module": [],
              "to_module": [],
              "to_original": "Branch",
              "to_renamed": "Branch",
              "site": "field:branch",
              "via_rename_reconcile": false
            },
            {
              "from": "topo_crate::Trunk",
              "to": "topo_crate::Leaf",
              "from_module": [],
              "to_module": [],
              "to_original": "Leaf",
              "to_renamed": "Leaf",
              "site": "field:leaf",
              "via_rename_reconcile": false
            }
          ],
          "sccs": [],
          "topo_groups": [
            {
              "level": 0,
              "members": [
                "topo_crate::Leaf"
              ]
            },
            {
              "level": 1,
              "members": [
                "topo_crate::Branch"
              ]
            },
            {
              "level": 2,
              "members": [
                "topo_crate::Trunk"
              ]
            }
          ],
          "diagnostics": []
        }
    "#]]
    .assert_eq(&render(&graph));
}
