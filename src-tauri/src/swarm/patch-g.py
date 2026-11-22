import io

p = 'acp/bridge.rs'
s = io.open(p, encoding='utf-8').read()

old = """                self.inner.emit_status().await;
                crate::settings::remember_workspace(&workspace);"""
new = """                self.inner.emit_status().await;
                // Only the window's own bridge decides where the window opens.
                // A swarm member starts a bridge of its own in its worktree, and
                // remembering that path made the app reopen in a folder the run
                // deletes: after the thirteenth live run the header read
                // `...\\.vibe\\worktrees\\run-...\\v-t2`, a verifier's checkout that
                // no longer existed. A scoped bridge is a member, not the user.
                if self.inner.scope.is_none() {
                    crate::settings::remember_workspace(&workspace);
                }"""
assert s.count(old) == 1
s = s.replace(old, new, 1)
io.open(p, 'w', encoding='utf-8', newline='').write(s)
print('ok')
