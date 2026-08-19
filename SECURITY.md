# Security

This is alpha software, not a production-secure communications device. Please
read this document before relying on it for anything sensitive.

## Known limitations

- Identities, credentials, configuration, journals, and message content are
  stored **without at-rest encryption**.
- Bluetooth bonding protects the nearby BLE link, and Reticulum protects
  application payloads end to end, but a public TCP peer can still observe
  connection metadata.
- Multi-user policy, credential revocation, secure backup, credential rotation,
  and background mobile operation are future work.
- The E290 reports requested SX1262 output power, not calibrated conducted
  power or antenna EIRP.

The full scope is recorded in
[`docs/architecture.md`](docs/architecture.md#security-boundary) and
[`docs/roadmap.md`](docs/roadmap.md#important-limitations).

## Reporting a vulnerability

Report suspected vulnerabilities privately rather than opening a public issue.
Use GitHub's **Report a vulnerability** flow on the repository, or email the
maintainers directly. Include:

- the affected firmware profile and app version;
- a minimal reproduction or description of the trigger;
- whether the finding is board-local, over-the-air, or in the client stack.

The maintainers will acknowledge the report and coordinate a fix and disclosure.
Please allow a reasonable window before publishing details of a confirmed
vulnerability.
