import valorIcon from "../assets/apps/valor.svg";

/** A program on the remote hosts that context menus can launch, restart and close (through
 *  each host's companion, in the signed-in user's session). */
export interface RemoteApp {
  id: string;
  name: string;
  /** Its executable on the remote; environment variables expand there. */
  path: string;
  /** An image URL for menus. */
  icon: string;
}

export const REMOTE_APPS: RemoteApp[] = [
  {
    id: "valor",
    name: "Valor",
    path: "%LOCALAPPDATA%\\Programs\\valor3\\Valor 3.0.exe",
    icon: valorIcon,
  },
];
