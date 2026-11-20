Fixed

- **Docs match the code for 0.10.0.** An audit before the release corrected
  stale names and numbers across the operator, schema, interface, test, and
  process docs:
  - log keys on `tip: accept`, the `run/*.addr` list, and RPC default ports
  - refusal lines that were removed with flat `*.idx.meta`, and the tx-head
    lookup order
  - four Esplora output-by-position routes added to `docs/esplora-openapi.yaml`
  - renamed tests and functions in the consensus and finding tables
  - the BIP54 rows in the BLVM crosswalk
