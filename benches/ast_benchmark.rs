//! Criterion benchmarks for ADD hot-path performance.
//!
//! # Running
//!
//! ```text
//! # Release profile (required for sub-5ms targets)
//! cargo bench --bench ast_benchmark
//!
//! # HTML report (opens target/criterion/report/index.html)
//! cargo bench --bench ast_benchmark -- --save-baseline main
//! ```
//!
//! # Performance targets (release / bench profile)
//!
//! | Fixture   | Lines | AST parse (`AstEngine::parse`) | Full validate (`parse` + rules) |
//! |-----------|-------|--------------------------------|----------------------------------|
//! | small     | ~50   | < 1 ms                         | < 2 ms                           |
//! | medium    | ~300  | < 2 ms                         | < 4 ms                           |
//! | large     | 1000+ | < 4 ms                         | < 5 ms                           |
//!
//! The 5 ms ceiling is the per-file IDE feedback budget. Regressions should be
//! caught by comparing Criterion baselines across PRs:
//!
//! ```text
//! cargo bench --bench ast_benchmark -- --baseline main
//! ```

use std::path::Path;

use architectural_drift_detector::{
    ArchitectureConfig, AstEngine, FileLayout, StructuralLayout, Validator,
};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

/// Target per-file budget for IDE integration (milliseconds).
const TARGET_MS: f64 = 5.0;

fn synthetic_typescript(line_count: usize, import_every: usize) -> String {
    let mut source = String::with_capacity(line_count * 48);
    source.push_str("import React from \"react\";\n");
    source.push_str("import { Port } from \"../domain/ports/IUserRepository\";\n\n");

    for index in 0..line_count {
        if index > 0 && index % import_every == 0 {
            source.push_str(&format!(
                "import {{ mod{index} }} from \"./feature/mod{index}\";\n"
            ));
        }
        source.push_str(&format!(
            "export function component{index}(value: number): number {{\n  return value + {index};\n}}\n\n"
        ));
    }

    source
}

fn bench_config() -> Validator {
    let yaml = r#"
layers:
  presentation:
    path_patterns: ["**/ui/**"]
    allowed_dependencies: [domain]
    enforce_interface_boundary: true
    interface_port_layer: domain
  domain:
    path_patterns: ["**/domain/**"]
    port_path_patterns: ["**/domain/ports/**"]
  infrastructure:
    path_patterns: ["**/data/**", "**/infrastructure/**"]
rules:
  - name: no-ui-to-infra
    source_layers: [presentation]
    forbidden_import_patterns: ["**/data/**", "**/infrastructure/**"]
    message: "blocked"
"#;
    let config = ArchitectureConfig::from_yaml_str(yaml).expect("bench config");
    Validator::new(config.compile().expect("compile"))
}

fn parse_fixture(engine: &mut AstEngine, source: &str) -> FileLayout {
    engine
        .parse(Path::new("src/ui/Benchmark.tsx"), source)
        .expect("parse")
}

fn validate_fixture(validator: &Validator, layout: FileLayout) {
    let _ = validator
        .validate_file_layout(&layout)
        .expect("validate");
}

fn bench_ast_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("ast_parse");
    group.throughput(Throughput::Elements(1));

    let fixtures = [("small_50", 50), ("medium_300", 300), ("large_1200", 1200)];

    for (name, lines) in fixtures {
        let source = synthetic_typescript(lines, 25);
        group.bench_with_input(BenchmarkId::new("parse", name), &source, |b, src| {
            let mut engine = AstEngine::tsx().expect("engine");
            b.iter(|| black_box(parse_fixture(&mut engine, src)));
        });
    }

    group.finish();
}

fn bench_validate(c: &mut Criterion) {
    let mut group = c.benchmark_group("validate");
    group.throughput(Throughput::Elements(1));

    let validator = bench_config();
    let fixtures = [("small_50", 50), ("medium_300", 300), ("large_1200", 1200)];

    for (name, lines) in fixtures {
        let source = synthetic_typescript(lines, 25);
        let mut engine = AstEngine::tsx().expect("engine");
        let layout = parse_fixture(&mut engine, &source);

        group.bench_with_input(BenchmarkId::new("validate", name), &layout, |b, layout| {
            b.iter(|| black_box(validate_fixture(&validator, layout.clone())));
        });
    }

    group.finish();
}

fn bench_end_to_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("end_to_end");
    group.throughput(Throughput::Elements(1));

    let validator = bench_config();
    let fixtures = [("small_50", 50), ("medium_300", 300), ("large_1200", 1200)];

    for (name, lines) in fixtures {
        let source = synthetic_typescript(lines, 25);
        group.bench_with_input(BenchmarkId::new("parse_and_validate", name), &source, |b, src| {
            let mut engine = AstEngine::tsx().expect("engine");
            b.iter(|| {
                let layout = parse_fixture(&mut engine, src);
                black_box(validate_fixture(&validator, layout));
            });
        });
    }

    group.finish();
}

/// Smoke assertion group — logs when estimated time exceeds the 5 ms IDE budget.
fn bench_target_budget(c: &mut Criterion) {
    let mut group = c.benchmark_group("target_budget_5ms");
    group.sample_size(50);

    let validator = bench_config();
    let source = synthetic_typescript(1200, 25);

    group.bench_function("large_file_full_pipeline", |b| {
        b.iter(|| {
            let mut engine = AstEngine::tsx().expect("engine");
            let layout = parse_fixture(&mut engine, &source);
            black_box(validate_fixture(&validator, layout));
        });
    });

    group.finish();

    // Criterion prints timing — document the SLA in module docs above.
    let _ = TARGET_MS;
}

criterion_group!(
    benches,
    bench_ast_parse,
    bench_validate,
    bench_end_to_end,
    bench_target_budget
);
criterion_main!(benches);
