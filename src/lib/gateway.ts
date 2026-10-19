import type { GatewayInput, GatewaySettings } from "./types";

/** The stored view of a gateway after applying UI input (passwords are write-only). */
export function gatewayFromInput(input: GatewayInput | null, prev: GatewaySettings | null): GatewaySettings | null {
  if (!input) return null;
  const hasPassword = input.password === undefined ? (prev?.hasPassword ?? false) : input.password !== "";
  return {
    enabled: input.enabled,
    host: input.host,
    sameCredentials: input.sameCredentials,
    username: input.username,
    domain: input.domain,
    hasPassword,
  };
}

/** Editable copy of stored gateway settings (password left unchanged). */
export function gatewayToInput(g: GatewaySettings | null): GatewayInput {
  return {
    enabled: g?.enabled ?? true,
    host: g?.host ?? "",
    sameCredentials: g?.sameCredentials ?? true,
    username: g?.username ?? "",
    domain: g?.domain ?? "",
  };
}

/** Short description of a gateway for hints. */
export function describeGateway(g: GatewaySettings | null): string {
  if (!g || !g.enabled || !g.host) return "Direct connection";
  return `via ${g.host}`;
}
