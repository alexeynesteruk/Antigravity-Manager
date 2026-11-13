# Consolidation for publication

The maintainer requested consolidation of all local branches/worktrees, commit,
and push. The repository and remote default branch are `main`; there is no
`master` branch. Exactly one worktree exists. The previous direct-main approval
continues to apply. No release tag, version bump or platform package is requested.

## Content and branch decisions

The pending implementation is committed as two functional units: gateway/account
hardening with its settings, compatibility documentation and focused regression
harnesses; and CI signature/change-scope verification with its helper fixtures.
Investigation reports and public metadata inventories form a separate record.

All other local development branch tips were already ancestors of `main` except
these four historical tips:

| Historical tip | Verified replacement already on main |
| --- | --- |
| `backup/integration-pre-tidy` (`b2cbe93a`) | `728e1d1e`: only whitespace/newline differences in `index.html` and `src/App.css`; whole-tree `git diff --ignore-all-space` is empty. |
| `backup/pre-migration-link-clarification-20261112` (`6cf98e26`) | `554d8970`: identical trees; later commit clarifies attribution. |
| `backup/pre-migration-attribution-correction-20261112` (`efdb8629`) | `554d8970`: identical trees; later commit corrects the source reference. |
| `pr-3643` (`e977df62`) | `5d4cb7eb`: identical stable backend patch IDs. The remaining `/graft/` ignore entry is already present. Later compatibility and media-safety fixes must remain. |

The final ancestry merge uses Git's `ours` strategy for these verified duplicate
tips. Its tree must equal the preceding reviewed tree. This records all local
branch ancestry without overwriting newer implementation or corrected metadata.
No upstream default branch, rejected fork patch or unreviewed feature is imported.
Local rollback refs, including `backup/pre-consolidation-20261112`, are retained.

## Validation and boundaries

Existing validation records were inspected rather than rerunning already-passing
checks without a new implementation change. The five-fix record reports 109
focused Rust tests, 10 settings checks, 6 automatic-refresh checks, TypeScript
checking, both binary test targets compiled, a successful headless build and 20
isolated native smoke checks with zero provider accounts/model attempts. Earlier
deep-review validation records include 48 focused Rust tests, 25 CI helper tests
and 21 frontend source checks. These sets overlap and are not added together.

Staged whitespace/conflict-marker checks and repository/ancestry checks accompany
publication. A targeted credential-pattern scan found only the documented public
localhost test-only private-key fixture. Live profile files, production keys,
credential stores and build outputs are outside the committed file list.

Pending text follows repository LF normalization. Export/report checksums were
refreshed for those committed bytes so a fresh checkout can verify them. The
review-time raw working-tree source fingerprint remains explicitly historical.

The [five newest local findings](review-next.md) and selectively recommended fork
integrations remain follow-ups; this consolidation does not claim to fix them.
Linux/macOS native runtime and live Google account behavior remain unverified.
Pushing main requests routine validation under the existing workflow; native
packaging/publication remains manual opt-in.
