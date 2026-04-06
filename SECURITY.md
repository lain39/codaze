# Security Policy

## Reporting

Do not open public GitHub issues for credential leaks, account takeover risks, refresh-token handling flaws, or request forgery issues.

Use one of these private paths instead:

- GitHub private vulnerability reporting, if it is enabled for the repository
- a non-public maintainer contact listed in the repository profile, release notes, or project homepage

If neither private path exists yet, add one before publishing the repository broadly.

## What Counts As Sensitive

Examples:

- refresh token disclosure
- access token disclosure
- account file exposure
- admin endpoint exposure outside loopback
- request smuggling, SSRF, or credential forwarding flaws
- bypasses that break the intended local-only trust boundary

## Operational Notes

- Treat account files as secrets.
- Do not attach live refresh tokens or access tokens to issues or pull requests.
- If you need a repro, redact tokens and any machine-local paths before sharing logs.
