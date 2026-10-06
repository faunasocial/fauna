# Renamed: `docs/goal/features/` is now `docs/goal/behavior/`

This directory was renamed on 2026-08-26 to free the word *features* for the
user-facing feature catalog (`docs/features/`, owner
`docs/goal/architecture/feature-catalog.md`). Every document that lived here is at
`../behavior/<same filename>`; a link into this directory from an older,
frozen document resolves one directory over. Nothing else may live here —
`goal-lint` (rule E) rejects any other file under this path.
