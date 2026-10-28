# DynamoRIO counter2 physical inputs

`./prepare.sh` retains the former DBT benchmark preparation through the actual
maintained `reverie-dynamorio/scripts/build-client.sh` producer. The source is
now the full pinned `third-party/dynamorio` fork, with its exact three children;
there is no retired first-party submodule activation command.

The source already contains the real
`reverie-dynamorio-counter2-exact` binary. Its legacy command mapping is:

- `reverie-dbt-dynamorio-path home` becomes `reverie-dynamorio-path home`.
- `REVERIE_DBT_CLIENT` becomes `REVERIE_DYNAMORIO_CLIENT`.
- `reverie-dbt-native/libreverie_dbt_client.so` becomes
  `reverie-dynamorio-native/libreverie_dynamorio_client.so`.
- `reverie-dbt-counter2-exact -- GUEST ARGS` becomes
  `reverie-dynamorio-counter2-exact -- GUEST ARGS`.

The generic counter2 Tool, corpus, timing driver, native-output checks and
nonzero exact-counter requirements remain in core. This file transfers physical
preparation and exact executable naming only. External benchmark orchestration
against those explicit artifacts remains open; this is not a new measurement or
an executed/qualified benchmark. No generic algorithm is copied here.
