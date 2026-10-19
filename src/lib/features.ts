/** Features that work in the engine/API but are hidden from the interface for now. */
export const FEATURES = {
  /** RD Gateway settings (per group and per connection). */
  gateway: false,
  /** A separate Domain field; `DOMAIN\user` in the username field still works. */
  domainField: false,
} as const;
