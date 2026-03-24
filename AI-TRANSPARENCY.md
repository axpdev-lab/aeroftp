# AI Transparency Statement

## Overview

AeroFTP is designed, architected and maintained by [axpnet](https://github.com/axpnet). AI tools were used extensively throughout development as productivity accelerators, always under strict human-defined specifications, patterns and review.

Every feature, design decision and architectural choice is human-driven. AI accelerated development; it did not direct it.

## How AI Tools Were Used

### Code Implementation

AI tools (primarily Claude Code, with Codex and Gemini for specific tasks) were used to write code **according to detailed specifications** provided by the developer. The workflow follows a consistent pattern:

1. **Human defines the specification**: feature scope, API design, data structures, security requirements, UI behavior
2. **AI generates implementation**: code is produced following the spec, project conventions, and existing patterns
3. **Human reviews line by line**: every generated file is reviewed, tested, and adjusted before committing
4. **Human commits with intent**: commit messages reflect deliberate, understood changes

The project maintains internal specification files with strict coding guidelines, architectural decisions, dependency pins, security patterns and release procedures. These act as a living specification that constrains AI output to match the project's standards.

### Translations (47 Languages)

AI tools made it possible for a solo developer to maintain 47 complete translations. The process:

- English (`en.json`) is the human-written reference
- Italian (`it.json`) is manually translated by the developer (native speaker)
- Remaining 45 languages are batch-translated by AI agents, then validated with automated scripts (`npm run i18n:validate`)
- Technical terms (FTP, SFTP, OAuth, AeroVault, AeroSync) are never translated
- Translation quality audits are performed periodically (e.g., v2.0.7 eliminated 605 silent intruder keys across 46 locales)

### Documentation

All documentation, including the CLI Guide, Protocol Features matrix, and this transparency statement, was drafted with AI assistance under human direction. The developer defines the structure, content scope and technical accuracy requirements; AI assists with prose and formatting.

### Code Review and Security Audits

Multi-agent security audits are a core part of the release process. Multiple independent AI reviewers (typically 3-5 per audit) analyze the codebase for:

- OWASP Top 10 vulnerabilities
- Rust-specific safety issues (TOCTOU, path traversal, OOM)
- Cryptographic implementation correctness
- Dependency vulnerability assessment

Over 500 audit findings have been identified and resolved across the project's history. The developer triages every finding, decides which are valid, and implements or rejects fixes based on technical judgment.

### Agent Testing

AeroFTP is designed as a first-class tool for AI agents (CLI with `--json` output, vault-based credential isolation, semantic exit codes). Consequently, AI agents are also used to test the application in realistic scenarios, validating the same workflows that end users and automated agents will execute in production.

## What AI Does NOT Do

- **Architecture decisions**: Protocol selection, encryption choices (AES-256-GCM-SIV, Argon2id), plugin system design, sync engine architecture, all human decisions
- **Security model**: Credential isolation, vault encryption, CSP policies, TOFU host key verification, all designed by the developer
- **Feature prioritization**: Roadmap, release scope, provider selection, all human-driven
- **User experience**: UI layout, theme system, keyboard shortcuts, modal behavior, all specified by the developer
- **Dependency choices**: Every crate and npm package is selected and pinned by the developer

## Verification

Reviewers and auditors can verify the human-driven development model by examining:

- **Git history**: Every commit has a descriptive message following Conventional Commits, reflecting deliberate changes
- **SPDX headers**: Every source file carries a license identifier and AI-assisted attribution
- **Security audit trail**: `docs/dev/` contains audit reports showing human triage of AI-generated findings
- **Issue tracker**: Feature requests, bug reports, and design discussions reflect human decision-making

## Tools Used

| Tool | Primary Use |
|------|-------------|
| Claude Code (Anthropic) | Code implementation, refactoring, security audits, translations |
| Codex (OpenAI) | Code review, alternative implementations, counter-audits |
| Gemini (Google) | Translations, documentation drafting |
| GLM (Zhipu) | Batch translation for CJK languages |

## Contact

For questions about AI usage in this project, open an issue on [GitHub](https://github.com/axpdev-lab/aeroftp/issues).

---

*This statement is part of AeroFTP's commitment to transparency in AI-assisted software development.*
