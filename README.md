# Architectural Drift Detector (ADD) 🚀

**Architectural Drift Detector (ADD)** is an ultra-high performance, lightweight developer tool and Language Server (LSP) written in Rust. It enforces strict architectural boundaries, sub-layer configurations, and clean architecture port/interface restrictions in real-time.

By parsing codebases using **Tree-sitter AST queries**, ADD guarantees a zero-latency feedback loop (<5ms per file) for developers while typing, and generates beautiful structural compliance dashboards for CI/CD pipelines.

---

## ✨ Key Features

- ⚡ **Blazing Fast Performance:** Written in Rust, utilizing a dynamic `DashMap` incremental AST cache and zero-copy string slicing. Runs under 5ms per file.
- 🌲 **Robust AST Parsing:** Captures all variants of imports (Static, CommonJS `require`, Dynamic `import()`, and `export ... from` re-exports).
- 🏗️ **Clean Architecture Enforcer:** Built-in support to restrict sub-layers from breaking the Dependency Inversion Principle (e.g., preventing UI layers from bypassing domain ports to access infrastructure directly).
- 🌐 **IDE Integration (LSP-Ready):** Implements the Language Server Protocol (`tower-lsp`) to stream live diagnostic squiggly lines (Errors/Warnings) straight to Cursor and VS Code.
- 📊 **Visual CI/CD Dashboards:** Generates responsive HTML reporting dashboards with a derived **Architecture Health Score** and GitHub Actions step summaries.

---

## 🏗️ Project Architecture

```mermaid
flowchart TD
    A[architecture.yaml] --> B[Rule Engine & Configuration]
    C[TS / TSX Source Files] --> D[Tree-sitter AST Engine]
    D --> E[Incremental Architecture Cache]
    B --> F[Validator]
    E --> F
    F --> G[LSP Diagnostics / Live IDE]
    F --> H[CI/CD Visual HTML/MD Dashboard]
