/**
 * Types representing an Atopile netlist structure
 * These types match the format found in demo.json
 */

/**
 * Module reference information
 */
export interface ModuleRef {
  source_path: string;
  module_name: string;
}

/**
 * Kind of instance in the netlist
 */
export enum InstanceKind {
  MODULE = "Module",
  COMPONENT = "Component",
  INTERFACE = "Interface",
  PORT = "Port",
}

/**
 * Represents the possible types of attribute values
 */
export interface AttributeValue {
  String?: string;
  Number?: number;
  Boolean?: boolean;
  Array?: AttributeValue[];
  Physical?: string;
}

/**
 * An instance in the netlist
 */
export interface Instance {
  type_ref: ModuleRef;
  kind: InstanceKind;
  attributes: Record<string, AttributeValue | string>; // Support both new AttributeValue and legacy string format
  children: Record<string, string>;
  reference_designator?: string | null;
}

/**
 * The complete netlist structure
 */
export interface Netlist {
  instances: Record<string, Instance>;
  nets: Record<string, Net>;
  root_ref: string;
  symbols?: Record<string, string>;
}

/**
 * Kind of net in the netlist
 */
export enum NetKind {
  NORMAL = "Normal",
  POWER = "Power",
  GROUND = "Ground",
}

/**
 * A net in the netlist
 */
export interface Net {
  kind: NetKind;
  name: string;
  ports: string[];
  properties?: Record<string, any>;
}
