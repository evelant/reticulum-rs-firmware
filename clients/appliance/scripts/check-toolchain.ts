import { assertExpectedBun } from "./toolchain.ts";

assertExpectedBun();
console.log(`Bun ${Bun.version} (${Bun.revision})`);
