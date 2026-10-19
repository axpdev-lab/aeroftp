# Synthetic profile-auth fixtures

`github-app-test-key.pem` is a public, test-only 2048-bit RSA key generated for
the loopback GitHub App lifecycle fixture. It has never been bound to a real
account, App or installation and must never be used outside tests.

The fixture lets the Rust test exercise actual RS256 signing without requiring
the OpenSSL executable at runtime. The lifecycle child runs with an empty PATH
to keep this guarantee explicit.
