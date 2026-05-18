# gitBuddy
Ein Buddy für den Überblick über all Deine GIT-Repos.

## Authentication

GitHub kann auf zwei Wegen verbunden werden:

- **Sign in with browser** (empfohlen) — OAuth Device Flow. App zeigt einen
  Code, du gibst ihn auf `github.com/login/device` ein. Kein Token-Setup
  nötig.
- **Personal access token** — der bestehende Pfad. Token mit Scopes
  `repo, read:org` auf github.com erzeugen und einfügen.

GitLab und Codeberg / Gitea / Forgejo nutzen aktuell ausschließlich PATs.

Hintergrund zu OAuth-App-Registrierung und Keychain-Layout: siehe
[docs/DECISIONS.md](docs/DECISIONS.md).
