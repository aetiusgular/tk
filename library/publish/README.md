# library/publish

`tk publish` (toolkit plan T3.3, lane G): put an entry into tk's library by
pull request, with the records a reader needs to judge it.

```
node library/publish/cli.ts publish <entry-dir> --library <your clone of aetiusgular/tk>
                            --proof github|dns|http [--key-file <path> | --key-stdin]
                            [--upstream <file>] [--open-pr] [--head-owner <login>]
node library/publish/cli.ts record (--key-file <path> | --key-stdin)
node library/publish/cli.ts verify --pr <clone> --base <clone> --author <login> <id>...
```

## What `publish` does

1. **Validates** the entry with the library's own rules (`library/validate/`):
   the schemas, upstream's rules, the reject list in any spelling,
   install-time execution and remote-code loading in the text it ships,
   lookalike names against the verified brands, and the catalog-wide rules
   with the entry joined to your clone's catalog.
2. **Catalog rules:** a version is immutable (the same version with other
   content is refused), a folder holds one name, a name lives in one folder,
   and `mirrored` is the mirror's tier, never a publisher's.
3. **Packages** (servers): for npm, refuses `preinstall`, `install`,
   `postinstall` and a node-gyp build, and records SLSA provenance when its
   subject is the tarball npm serves and its source is the entry's
   repository; for PyPI, refuses a release with no wheel and records a PEP 740
   attestation on the same terms. Other package kinds (OCI, NuGet, mcpb) are
   recorded as not checked. tk records what an attestation says and checks it
   is about this package; it does not verify the Sigstore chain
   (`npm audit signatures` does).
4. **Namespace**, as the official registry decides it:
   - `io.github.<login>/*`: `--proof github`; `gh`'s logged-in user must be
     the login, or an active admin of the org. The token stays inside `gh`.
   - `<reverse domain>/*`: `--proof dns` (a `v=MCPv1; k=<ed25519|ecdsap384>; p=...`
     TXT record on the domain or a parent) or `--proof http`
     (`https://<domain>/.well-known/mcp-registry-auth`, the domain only). You
     sign a statement naming the entry, its version and its content hash with
     the key that record names, the same key `mcp-publisher login dns|http`
     uses. `record` prints the line for a key.
   - github.io never proves a namespace; IP addresses and single labels never do.
5. **Records** `contentSha256` (SHA-256 of the RFC 8785 form of the manifest
   without tk's trust record, plus every other file in the folder),
   `namespaceProof` and `provenance` into the manifest, and writes
   `publish-proof.json` beside it.
6. **The pull request**, in your clone: a branch `publish/<id>-<version>`, the
   folder copied to `library/catalog/<id>/`, one commit in your git identity,
   a push of that branch, `gh pr create --repo aetiusgular/tk`. Printed by
   default; `--open-pr` runs it. Every step is an argument list.
7. **Upstream** (`--upstream <file>`): a copy of server.json with tk's object
   trimmed until the publisher `_meta` object fits the official registry's
   4,096 bytes as Go measures it, and the `mcp-publisher` lines to publish it
   with your own login. tk never publishes upstream and never opens a pull
   request against `modelcontextprotocol/registry` (its README: publishing
   never happens there by pull request).

A private key is read from a file (`--key-file`, warned on when others can
read it) or stdin (`--key-stdin`), never from argv, and nothing of it is printed.

## What the library's CI does

`ci/verify-pr.yml` (a proposal integration installs) runs `verify` from the
base branch's code over the pull request's files: the rules again, the hash
recomputed, the namespace again (the pull request's author for
`io.github.*`, the domain's live key for a signature), the packages again.
An org namespace opened by someone other than the org's name is a warning for
a reviewer: the workflow's token cannot read another user's org role.
Versions are immutable; a takedown is `status: deleted` with the metadata
kept (T4.4).
