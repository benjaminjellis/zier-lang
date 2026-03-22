<p align="center">
  <img src="assets/mond.png" alt="Mond logo" width="160" />
</p>

`Mond` is an experimental functional language with a Lisp-inspired syntax and ML-style static types that targets the BEAM.

To get started read the [book](https://benjaminjellis.github.io/mond)

This repo is a mono-repo that contains the core of the `Mond` programming language, including:
- `bahn` - the build tool for the `Mond` programming language
- `mond-format` - a library for formatting `Mond` source code
- `mond-lsp` - a library for the `Mond` lsp (language server protocol)
- `mondc` - the compiler for the `Mond` programming language
- `book` - the `Mond` programming language book
- `samples` - some `Mond` samples

Other parts of the ecosystem are hosted in separate repos:
- [standard library](https://github.com/benjaminjellis/mond-std)
- [otp](https://github.com/benjaminjellis/otp)

## AI Policy
`Mond` has proved a useful project in exploring using LLMs / agentic AI for increasing productivity. At rough estimate the repo is currently 70% LLM generated and 30% human written. All AI generated code is reviewed by a human and considerable effort has been made on code coverage and ensuring that the AI generated code behaves as intended.
