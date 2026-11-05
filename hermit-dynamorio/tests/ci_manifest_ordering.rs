/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 * All rights reserved.
 *
 * This source code is licensed under the BSD-style license found in the
 * LICENSE file in the root directory of this source tree.
 */

//! Slice S13 diagnostic ordering, transferred from the core manifest-plan
//! integration target without changing its hosted-profile or 16/18 assertions.
//! The population comes from the existing core planner for this execution owner;
//! the graph is the actual generated extension graph consumed by prepared Nextest.
//! Missing graph, preparation, source or planner is a failure, never a skip.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value as JsonValue;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("hermit-dynamorio must sit below the extension repository root")
        .to_path_buf()
}

fn read_json(path: &str) -> JsonValue {
    let path = repo_root().join(path);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()))
}

fn field<'a>(row: &'a JsonValue, name: &str) -> &'a str {
    row.get(name)
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("row has no string {name}: {row}"))
}

fn explicit_directory(name: &str) -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")));
    assert!(
        path.is_absolute(),
        "{name} is not absolute: {}",
        path.display()
    );
    assert_eq!(
        std::fs::canonicalize(&path).unwrap(),
        path,
        "{name} is not canonical"
    );
    assert!(
        path.is_dir(),
        "{name} is not a directory: {}",
        path.display()
    );
    path
}

fn actual_tag(row: &JsonValue) -> String {
    format!("{}.{}", field(row, "group"), field(row, "job"))
}

// Only source ownership renamed these group names. The two diagnostic guards
// and the isolation aggregate retain their original physical node identifiers.
fn logical_tag(row: &JsonValue) -> String {
    let group = field(row, "group");
    format!(
        "{}.{}",
        group.strip_prefix("dynamorio-").unwrap_or(group),
        field(row, "job")
    )
}

/// `(lane, category)` of every actual CI-selected DynamoRIO plan cell.
fn dbt_plan_buckets() -> BTreeSet<(String, String)> {
    let root = repo_root();
    assert_eq!(explicit_directory("HERMIT_EXTENSION_ROOT"), root);
    assert_eq!(
        std::env::var("HERMIT_PREPARED_NEXTEST_REQUIRED").unwrap(),
        "1"
    );
    let core = explicit_directory("HERMIT_CORE_SOURCE");
    let tools = explicit_directory("HERMIT_CORE_TOOLS");
    assert_ne!(
        core, root,
        "the core provider and extension source must be distinct"
    );
    assert!(
        tools.starts_with(core.join("target")),
        "planner tools are outside the original core output"
    );
    let descriptor = PathBuf::from(
        std::env::var_os("HERMIT_NEXTEST_PROJECT")
            .expect("actual prepared project descriptor required"),
    );
    assert!(
        descriptor == root.join("ci/prepared-project-host.json")
            || descriptor == root.join("ci/prepared-project-pinned.json")
    );
    let descriptor: JsonValue =
        serde_json::from_slice(&std::fs::read(descriptor).unwrap()).unwrap();
    assert_eq!(descriptor["schema"], 1);
    assert_eq!(descriptor["repository"], root.to_str().unwrap());
    assert_eq!(
        descriptor["graph"],
        root.join("ci/dag/validate.json").to_str().unwrap()
    );
    assert_eq!(descriptor["tools_root"], core.to_str().unwrap());
    assert_eq!(descriptor["shared_package"], "hermit-run");

    // Reuse the actual manifest parser/owner partition and complete expected-plan
    // provider. This does not run Hermit, a guest, a compiler or an SDK producer.
    // The required preparation binds the original core tool cohort; there is no
    // PATH lookup, copied manifest policy, retained mixed plan or source fallback.
    let output = Command::new(tools.join("test-harness"))
        .args([
            "expected-plan",
            "--execution-owner",
            "dynamorio",
            "--repo-root",
        ])
        .arg(&core)
        .current_dir(&core)
        .output()
        .expect("cannot run the required current core plan provider");
    assert!(
        output.status.success(),
        "current owner plan refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: JsonValue = serde_json::from_slice(&output.stdout)
        .expect("current owner provider returned invalid JSON");
    let cells = plan["cells"]
        .as_array()
        .expect("current owner plan has no cells");
    assert!(!cells.is_empty(), "current owner provider selected no cell");
    let catalog = read_json("ci/retained-ci-catalog.json");
    assert_eq!(catalog["schema"], 1);
    assert_eq!(catalog["owner"], "dynamorio");
    let dag = read_json("ci/dag/validate.json");
    let mut by_tag = BTreeMap::new();
    for row in dag["steps"].as_array().unwrap() {
        assert!(
            by_tag.insert(actual_tag(row), row).is_none(),
            "duplicate actual graph tag"
        );
    }
    let mut source_tags = BTreeSet::new();
    for row in catalog["steps"].as_array().unwrap() {
        let tag = actual_tag(row);
        assert!(
            source_tags.insert(tag.clone()),
            "duplicate source catalog tag: {tag}"
        );
        let actual = by_tag.get(&tag).unwrap_or_else(|| {
            panic!("source owner {tag} missing from the actual generated graph")
        });
        // The producer adds only real context/custody environment and quoting.
        // Every original dependency, profile, result owner and complete resource
        // policy must survive emission exactly, including both diagnostic guards.
        for key in [
            "manifest",
            "labels",
            "deps",
            "timeout",
            "cpu_timeout",
            "jobs_flag",
            "jobs_env",
            "hint",
            "result_manifests",
        ] {
            assert_eq!(
                actual.get(key),
                row.get(key),
                "{tag} changed source policy {key}"
            );
        }
        if row.get("manifest").is_some() {
            assert_eq!(
                flag_value(field(actual, "cmd"), "--execution-owner"),
                Some("dynamorio"),
                "{tag} lost its actual owner selection"
            );
        }
    }
    cells
        .iter()
        .map(|cell| {
            assert_eq!(
                field(cell, "backend"),
                "dbt",
                "provider crossed execution owners: {cell}"
            );
            (field(cell, "lane").into(), field(cell, "category").into())
        })
        .collect()
}

/// The value after `flag` in a node command, if the flag appears once.
fn flag_value<'a>(cmd: &'a str, flag: &str) -> Option<&'a str> {
    let mut words = cmd.split_whitespace();
    words.position(|word| word == flag)?;
    words.next()
}

/// The retired test.dbt_parity node depended on check.dbt_runtime_abi so the
/// ABI check ran first and named a missing DBT runtime callback before
/// eager-exit could cancel it. Its cases now run in manifest nodes, so every
/// manifest node that selects a DBT cell and shares a dagrun profile with the
/// check has to carry the same ordering, and no other node needs it.
///
/// `hosted-portable` is not such a profile: the hosted workflow runs each E2E
/// node as its own job through ci/run-node.sh, which omits dependencies outside
/// the job's selection, and the ABI check runs in a separate release-shard job
/// that an E2E failure cannot cancel. ci/check-shard-coverage.sh refuses an E2E
/// edge to a node that no earlier hosted job supplies, so the _on_host twins
/// must not carry it.
#[test]
fn every_manifest_node_with_a_dbt_cell_orders_after_the_dbt_runtime_abi_check() {
    const ABI: &str = "check.dbt_runtime_abi";
    const HOSTED_PORTABLE: &str = "hosted-portable";
    let dag = read_json("ci/dag/validate.json");
    let steps = dag["steps"].as_array().unwrap();
    let tag = |step: &JsonValue| logical_tag(step);
    let labels = |step: &JsonValue| {
        step["labels"]
            .as_array()
            .unwrap()
            .iter()
            .map(|label| label.as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>()
    };
    let abi = steps
        .iter()
        .find(|step| tag(step) == ABI)
        .unwrap_or_else(|| panic!("{ABI} is missing from ci/dag/validate.json"));
    // Since the one-build change of 2026-09-30 the local check runs in the
    // pinned root, so hosted-portable runs its own host twin, which the hosted
    // E2E twins must still not depend on (see above).
    let hosted_abi = format!("{ABI}_on_host");
    let hosted = steps
        .iter()
        .find(|step| tag(step) == hosted_abi)
        .unwrap_or_else(|| panic!("{hosted_abi} is missing from ci/dag/validate.json"));
    assert_eq!(
        labels(hosted),
        BTreeSet::from([HOSTED_PORTABLE.to_string()]),
        "{hosted_abi} left the hosted-portable profile; revisit the _on_host twins"
    );
    let abi_labels = labels(abi);
    assert!(
        !abi_labels.contains(HOSTED_PORTABLE),
        "{ABI} is selected by hosted-portable besides its host twin {hosted_abi}"
    );
    assert!(!abi_labels.is_empty(), "{ABI} is in no dagrun profile");
    assert!(
        !steps.iter().any(|step| {
            step["deps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dep| dep.as_str() == Some(hosted_abi.as_str()))
        }),
        "a node depends on {hosted_abi}; the hosted E2E twins must not"
    );
    let dbt = dbt_plan_buckets();
    assert!(!dbt.is_empty(), "the committed plan selects no DBT cell");
    let mut needs = BTreeSet::new();
    let mut has = BTreeSet::new();
    for step in steps {
        let cmd = field(step, "cmd");
        if step["deps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|dep| dep.as_str() == Some(ABI))
        {
            has.insert(tag(step));
        }
        if !cmd.contains("--ci-only") || flag_value(cmd, "--execution-owner") != Some("dynamorio") {
            continue;
        }
        let (Some(lane), Some(category)) =
            (flag_value(cmd, "--lane"), flag_value(cmd, "--category"))
        else {
            continue;
        };
        if dbt.contains(&(lane.to_string(), category.to_string()))
            && !labels(step).is_disjoint(&abi_labels)
        {
            needs.insert(tag(step));
        }
    }
    assert_eq!(
        needs,
        BTreeSet::from([
            "e2e.manifest_applications".to_string(),
            "e2e.manifest_bin_c".to_string(),
            "e2e.manifest_c_programs".to_string(),
            "e2e.manifest_chaos_c".to_string(),
            "e2e.manifest_compat".to_string(),
            "e2e.manifest_data_handling".to_string(),
            "e2e.manifest_debugger_c".to_string(),
            "e2e.manifest_determinism_stress".to_string(),
            "e2e.manifest_determinism_stress_c".to_string(),
            "e2e.manifest_language_runtimes".to_string(),
            "e2e.manifest_shared_futex_c".to_string(),
            "e2e.manifest_system_utils".to_string(),
            "e2e.manifest_util_c".to_string(),
            "privileged-e2e.manifest_applications".to_string(),
            "privileged-e2e.manifest_c_programs".to_string(),
            "privileged-e2e.manifest_system_utils".to_string(),
        ])
    );
    // This counted integration target runs real DBT cases outside the
    // manifest buckets, so it must also wait for the same ABI check.
    let mut expected_dependents = needs;
    expected_dependents.insert("test.inherited_terminal_read".to_string());
    // So does the isolation matrix node that runs the DBT cells.
    expected_dependents.insert("test.isolation_matrix_dbt_in_guest".to_string());
    assert_eq!(has, expected_dependents);
}
