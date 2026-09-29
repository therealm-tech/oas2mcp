import { readBuildTarget } from "./docs-versions.ts";

export const buildTarget = readBuildTarget(process.env);
