# MCP recursive path policy

`aeroftp_sync_tree` and `aeroftp_check_tree` install an immutable local-path scope before scanning. Choosing an allowed ancestor does not grant access to sensitive descendants. The shared MCP denylist covers system paths and credential/application directories under the captured home. Missing download parents are checked through their deepest existing ancestor. Unresolved paths, dangling links and ambiguous Windows names fail closed.

Protected-prefix comparison folds case on Windows and non-Linux Unix systems, including macOS, so missing destinations such as `.SSH/new/key` cannot bypass a protected `.ssh` prefix. Linux retains native case-sensitive comparison. Protected alias resolutions are refreshed once per admission and shared only by that admission's ordered prefixes; every later scan check, retry and side effect refreshes them again.

Local walks check before descending or hashing. Remote walks check projected local prefixes before listing descendants or requesting checksums and use BFS rather than a provider's flat recursive listing. Denied relative prefixes are reported with `policy_denied` and excluded symmetrically from both inventories, so a refused subtree cannot be treated as a missing counterpart for orphan deletion. `summary_only` retains boundary totals and policy denial counts.

`check_tree` preserves partial comparisons and incomplete-scan fields for ordinary scan failures. Unbounded policy refusals remain errors, including when the boundary budget cannot represent a denied counterpart. Mutating sync continues to refuse every unbounded scan.

Transfers recheck the same scope at admission, including clone workers, delta retries, native bulk and local/remote deletion. A mixed bulk group refuses denied members terminally and sends allowed members through the guarded single-file path. Callers without an injected scope retain ordinary GUI/CLI behavior.

Restricted sync rejects error correction before scanning: the current EC scratch/repair APIs do not carry this filesystem scope. MCP sync already disables EC. A raw encrypted comparison must use a provider that exposes plaintext names through its crypt overlay; otherwise restricted check refuses it before recursion.

These are pathname policy checks, not atomic filesystem capabilities. They catch deterministic alias changes before admission, but a hostile same-user process can still race an adapter's later open. Hard-link identity policy and descriptor-based provider I/O are separate work.
