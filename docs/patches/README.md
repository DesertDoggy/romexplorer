# Patches for the build submodules

Changes to this project's build scripts, kept here for reference. The build scripts now live
in `deps/<name>/scripts/`; the patches below were written when they lived in each fork's
`user/scripts/`.

**`mariadb-server-collect-share.patch` is already applied:**
`deps/mariadb-server/scripts/build_mariadb_server.sh` collects `share/` in
`collect_artifacts`. What follows is kept as the record of why.

## `mariadb-server-collect-share.patch`

**Fixes:** the Windows server and desktop packages failing `scripts/package.sh`'s gate with
*"bundled MariaDB is incomplete: missing share/english/errmsg.sys"*, and the same tree
failing `database::server::check_layout` at first start.

**The chain.** `build_mariadb_server.sh` builds, then runs `cmake --install --prefix
<stage_dir>`, which does produce `share/` — the bootstrap SQL, `share/english/errmsg.sys`
and `share/charsets/`. But `collect_artifacts` promotes only `bin/`, `lib*/` and `include/`
into the durable `deps/mariadb-server/release/<platform>/<arch>/<version>/` tree. `share/` is never copied
anywhere permanent, so the only copy stays in `<stage_dir>` — which lives under
`deps/mariadb-server/_build/` (scratch), and which each build deletes with `rm -rf "${stage_dir}"` before
it starts.

So whether `scripts/move_libs_to_app_root.sh` finds `share/` depends entirely on whether it
runs before the next build wipes the staging tree. On Linux and macOS it usually does,
because deploy follows build in the same sitting. On Windows it did not, and the result was
a package that looks complete — `mariadbd` present, plugins present — and cannot start.

**The fix** is the one `CODING_RULES.md` already calls for under *Native binaries and app
layout*: collect `share/` in `collect_artifacts`, alongside `bin/` and `shared/`. It is
curated the same way `BIN_KEEP_LIST` is — MariaDB installs about 25 localisations of
`errmsg.sys`, and the server is always started with `--lc-messages-dir` pointing at this
directory and `--character-set-server=utf8mb4`, so English plus the charsets plus the
bootstrap SQL is what is actually read. It also fails the build loudly if `share/` or
`errmsg.sys` is absent, rather than producing a release tree that cannot start.

**Applying it:**

```sh
# the patch was written against the fork's user/scripts/; strip that prefix
git apply -p3 --directory=deps/mariadb-server/scripts docs/patches/mariadb-server-collect-share.patch
# then rebuild for the platform that needs it, e.g.
deps/mariadb-server/scripts/build_mariadb_server.sh --platform windows
```

Then re-run `scripts/move_libs_to_app_root.sh`, which already prefers the release tree and
falls back to the staging tree, so it works before and after this patch lands.

The fallback branch in `move_libs_to_app_root.sh` can go once every platform's release tree
has been rebuilt with it.

### A workaround that needs no Windows build host

Confirmed by running it: `deps/mariadb-server/release/windows/x64/13.2.0/` is
already checked in and carries `bin`, `shared`, `include` and `build-info.txt` -- everything
except `share`. The Linux tree at the same path has the *same* three directories, so this is
not a Windows quirk at all. `share/` only ever reaches `app_root_ex` from the ephemeral
staging tree, which is why Linux appears to work: deploy usually follows build in one sitting.
Clean `deps/mariadb-server/_build` and re-run the deploy and Linux breaks identically.

`share/` turns out to be portable. It is 5.6 MB of XML charset definitions, bootstrap `.sql`
text and per-language `errmsg.sys` message files -- 39 entries, no ELF, nothing
architecture-specific. `errmsg.sys` is version-specific, not OS-specific, and both release
trees here are MariaDB 13.2.0. So a Windows tree can borrow the Linux one:

```sh
cp -a app_root_ex/lib/linux/x64/mariadb/share app_root_ex/lib/windows/x64/mariadb/
```

With that in place `scripts/package.sh --kind server --os windows --arch x64` stages and
clears its bundled-MariaDB gate (verified; 53 MB staged).

**What that does and does not prove.** It proves the packaging gate is satisfied and that the
files are not architecture-specific. It does *not* prove `mariadbd.exe` bootstraps from them,
which needs a Windows host to check -- run `romexplorer_dbtool check` and then a first start
there before trusting it. Treat this as a way to unblock building and testing Windows
packages now, not as a substitute for the patch above: the patch is what stops the problem
recurring on every platform after a clean.

