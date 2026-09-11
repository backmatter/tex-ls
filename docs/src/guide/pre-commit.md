# Pre-commit

Install Meaning from source, then use local [pre-commit](https://pre-commit.com)
hooks with the executable on your `PATH`:

```yaml
repos:
  - repo: local
    hooks:
      - id: meaning-lint
        name: Meaning lint
        entry: meaning lint
        language: system
        files: '\.(tex|sty|cls|dtx|ins|bib)$'
      - id: meaning-format
        name: Meaning format
        entry: meaning format --check
        language: system
        files: '\.(tex|sty|cls|dtx|ins|bib)$'
```

These hooks check files without changing them. Run `meaning format` explicitly
to apply formatting.
