// The one pair of icons used everywhere a session is started or stopped.

import { Play, Square } from "lucide-react";

/**
 * Connect: a green play triangle (outline, like every other icon). `plain` takes the
 * text color instead, for filled buttons where green would clash.
 */
export function ConnectIcon({ size = 14, plain }: { size?: number; plain?: boolean }) {
  return (
    <Play size={size} strokeLinejoin="round" style={plain ? undefined : { color: "var(--success)" }} aria-hidden />
  );
}

/** Disconnect: a red stop square (outline). */
export function DisconnectIcon({ size = 14 }: { size?: number }) {
  return (
    <Square size={Math.max(8, size - 1)} style={{ color: "var(--danger)" }} aria-hidden />
  );
}
