# Contributing to AeroFTP

First off, thank you for considering contributing to AeroFTP!

## Code of Conduct

Be respectful, inclusive, and professional. We're here to build great software together.

## How Can I Contribute?

### Reporting Bugs

- Use the issue tracker
- Include steps to reproduce
- Describe expected vs actual behavior
- Include screenshots if relevant

### Suggesting Features

- Check if the feature was already requested
- Describe the use case clearly
- Explain why this would be useful

### Pull Requests

1. Fork the repo
2. Create a feature branch (`git checkout -b feature/amazing-feature`)
3. Make your changes
4. Run tests (`cd src-tauri && cargo test`)
5. Commit (`git commit -m 'Add amazing feature'`)
6. Push (`git push origin feature/amazing-feature`)
7. Open a Pull Request

## Development Setup

```bash
# Clone the repo
git clone https://github.com/axpnet/aeroftp.git
cd aeroftp

# Install dependencies
npm install

# Run in development mode
npm run tauri dev

# Build
npm run tauri build
```

## Code Style

- Use TypeScript for frontend code
- Use Rust for backend (Tauri) code
- Follow existing code patterns
- Add comments for complex logic
- Keep functions small and focused

## Commit Messages

- Use clear, descriptive messages
- Start with a verb (Add, Fix, Update, Remove)
- Reference issues when relevant (#123)

## Test Requirements

All pull requests should include tests for new features and bug fixes where applicable:

- **Backend (Rust)**: Add unit tests in `#[cfg(test)]` modules. Run with `cargo test` from the `src-tauri/` directory.
- **Security checks**: Run `npm run security:regression` to verify security invariants.
- **i18n**: Run `npm run i18n:validate` to ensure all translation keys are present in all 47 languages.
- **Type checking**: Run `npx tsc --noEmit` to verify TypeScript types.

Pull requests that reduce test coverage or break existing tests will not be merged.

## Response Times

- **Bug reports**: We aim to acknowledge bug reports within 7 days.
- **Security vulnerabilities**: We respond within 48 hours (see [SECURITY.md](SECURITY.md) for details).
- **Pull requests**: We aim to review pull requests within 14 days.

## Questions?

Open a discussion or reach out to the maintainers.

Thank you for contributing!
