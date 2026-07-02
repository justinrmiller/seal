# Seal — Technical Reference (docs)

This directory holds a long-form, book-style technical reference for the **seal**
end-to-end-encrypted chat server, authored in LaTeX and built into **PDF** and
**EPUB**.

## Artifacts

| File | Format | Description |
|------|--------|-------------|
| [`seal.pdf`](seal.pdf) | PDF | Print/screen reference with linked table of contents. |
| [`seal.epub`](seal.epub) | EPUB | E-reader edition. |

Both are generated from the LaTeX source under [`latex/`](latex/) and committed
alongside it.

## Source layout

```
docs/
├── Makefile              # build rules (pdf / epub / all / clean)
├── README.md             # this file
├── seal.pdf              # built artifact
├── seal.epub             # built artifact
└── latex/
    ├── seal.tex          # main document: preamble, title page, \include chapters
    └── chapters/
        ├── 01-introduction.tex
        ├── 02-architecture.tex
        ├── 03-configuration.tex
        ├── 04-database.tex
        ├── 05-http-api.tex
        ├── 06-websocket.tex
        ├── 07-encryption-security.tex
        ├── 08-source-reference.tex
        ├── 09-testing.tex
        ├── 10-build-deploy.tex
        └── 11-object-storage.tex
```

## Building

Requires [`tectonic`](https://tectonic-typesetting.github.io/) (LaTeX → PDF) and
[`pandoc`](https://pandoc.org/) (→ EPUB):

```bash
brew install tectonic pandoc
```

Then, from this directory:

```bash
make all      # build both seal.pdf and seal.epub
make pdf      # just the PDF (tectonic)
make epub     # just the EPUB (pandoc)
make clean    # remove built artifacts
```

Or from the repository root: `make -C docs all`.

## Scope note

The reference documents the code on the `main` branch. The object-storage backend
(S3/GCS/Azure) lives on the unmerged `feat/object-storage` branch; it has its own
chapter, which clearly marks the feature-branch-only parts.
