# What Rookery hands to Ansible

Each `NAME.profile.json` is a made-up host's profile, and `NAME.vars.json` the variables Rookery gives the role for it (`apply::ansible_vars`). A test in `crates/rookery-core/src/apply.rs` compares the two. Write the variables again with `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-core ansible_contract`.

CI runs the end-to-end tests that apply the role on a pull request only when these files, the role or the code in `SEAM` of `ci/plan.py` change. A change to what the role gets shows here, so it runs them.
