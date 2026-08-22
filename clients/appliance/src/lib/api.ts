import type { ApplianceClient } from "./appliance-client.ts";
import { configuredApiOrigin, HttpApplianceClient } from "./http-appliance-client.ts";
import { NativeApplianceClient } from "./native-appliance-client.ts";

type ApplianceClientConstructor = new () => ApplianceClient;

/**
 * Native builds own their local Rust/SQLite runtime by default. Keeping an
 * explicit HTTP origin preserves the development-server adapter. The native
 * runtime uses its app-owned PRNS node and typed management application.
 */
export const ApplianceApi: ApplianceClientConstructor =
  configuredApiOrigin().length === 0 ? NativeApplianceClient : HttpApplianceClient;

export { configuredApiOrigin };
