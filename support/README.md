# Shared textual inputs

These four files are byte-for-byte copies from Reverie
[`882c91c02b981f56370a2912f0832b04fefc46ec`](https://github.com/rrnewton/reverie/commit/882c91c02b981f56370a2912f0832b04fefc46ec):

- `reverie-examples/counter1_tool.rs`
- `reverie-examples/counter2_tool.rs`
- `reverie-examples/recorder_tool.rs`
- `tests/c_tests/threads_group_exit_blocking.c`

The moved backend already included these Tool modules textually and used the C
fixture. Their generic source ownership stays with core Reverie. The temporary
copies close those exact inputs without depending on the old examples package,
whose manifest also depends on unrelated backends. Their behavior and assertions
are unchanged. Future publication must retain their source binding or provide a
reviewed generic core module with the same contract.

The original Reverie BSD-2-Clause license is included as [LICENSE](LICENSE).
