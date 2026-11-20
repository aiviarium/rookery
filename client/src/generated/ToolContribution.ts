// This file was ported from the Crow prototype and has no Rust type in rookery-proto yet.
import type { ServiceHandler } from "./ServiceHandler";

/**
 * A tool contribution (`contributes.tools`), reserved for a later version: this Rookery has no MCP
 * server, so it accepts the declaration and drops it when the plugin loads, and the host half's
 * `rookery.agents.tool` is not served either.
 */
export type ToolContribution = { 
/**
 * `<plugin>.<name>`.
 */
id: string, description: string, title?: string, 
/**
 * The agent's call becomes an approval card first.
 */
confirm: boolean, 
/**
 * A method of one of the plugin's app services that runs the tool instead of the host half. Its params
 * schema is the method's.
 */
handler?: ServiceHandler, };
