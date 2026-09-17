# Security Policy

## Supported versions

Only the current `main` branch receives security fixes.

## Reporting a vulnerability

Use GitHub's [private vulnerability reporting](https://github.com/omnibus-library/kobo-backup/security/advisories/new)
to report security issues confidentially. **Do not open a public issue for a
security vulnerability.**

What to expect:

- Acknowledgement within 3 business days.
- A status update within 14 days.
- Up to 90 days for investigation, remediation, and coordinated disclosure
  before details are published. Critical vulnerabilities that put users' data
  at active risk may be disclosed and fixed sooner.

## Scope

kobo-backup reads from and writes to a connected e-reader and to backup
archives on disk. Anything that could cause silent data loss, write outside
the chosen backup folder, or restore something other than what the archive
verifiably contains is in scope.
