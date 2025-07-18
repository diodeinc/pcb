"use client";

import React, { useState, useEffect, useCallback } from "react";
import { Loader, AlertCircle } from "react-feather";
import "./SchematicSidebar.css";

// Types
interface Parameter {
  name: string;
  param_type: string;
  required: boolean;
  is_config: boolean;
  is_enum: boolean;
  enum_values?: string[];
  type_info: {
    kind: string;
    name?: string;
    variants?: string[];
    element?: any;
    key?: any;
    value?: any;
    fields?: Record<string, any>;
    pins?: Record<string, any>;
    type_name?: string;
  };
}

interface DiagnosticInfo {
  level: string;
  message: string;
  file?: string;
  line?: number;
  child?: DiagnosticInfo;
}

interface SchematicSidebarProps {
  moduleName: string;
  filePath: string;
  parameters: Parameter[];
  diagnostics?: DiagnosticInfo[];
  error?: string | null;
  isEvaluating?: boolean;
  initialValues?: Record<string, string>;
  onInputChange?: (paramName: string, value: string) => void;
  onInputsChange?: (inputs: Record<string, string>) => void;
}

// Helper function to format type information
function formatTypeInfo(param: Parameter): string {
  const typeInfo = param.type_info;

  if (param.is_enum && param.enum_values) {
    return `enum[${param.enum_values.join(" | ")}]`;
  }

  switch (typeInfo.kind) {
    case "dict":
      const keyType = typeInfo.key?.kind || "any";
      const valueType = typeInfo.value?.kind || "any";
      return `dict[${keyType}, ${valueType}]`;
    case "list":
      const elementType = typeInfo.element?.kind || "any";
      return `list[${elementType}]`;
    case "interface":
      return typeInfo.name || "interface";
    case "record":
      return typeInfo.name || "record";
    default:
      return typeInfo.kind || param.param_type;
  }
}

export const SchematicSidebar: React.FC<SchematicSidebarProps> = ({
  moduleName,
  filePath,
  parameters,
  diagnostics = [],
  error = null,
  isEvaluating = false,
  initialValues = {},
  onInputChange,
  onInputsChange,
}) => {
  const [inputValues, setInputValues] =
    useState<Record<string, string>>(initialValues);

  // Initialize input values with defaults when parameters change
  useEffect(() => {
    const defaults: Record<string, string> = {};
    parameters.forEach((param: Parameter) => {
      // Skip if we already have a value for this parameter
      if (inputValues[param.name] !== undefined) {
        return;
      }

      const typeInfo = param.type_info;

      if (param.is_enum && param.enum_values && param.enum_values.length > 0) {
        defaults[param.name] = param.enum_values[0];
      } else if (typeInfo.kind === "string") {
        defaults[param.name] = "";
      } else if (typeInfo.kind === "int") {
        defaults[param.name] = "0";
      } else if (typeInfo.kind === "float") {
        defaults[param.name] = "0.0";
      } else if (typeInfo.kind === "bool") {
        defaults[param.name] = "false";
      } else if (typeInfo.kind === "list") {
        defaults[param.name] = "[]";
      } else if (typeInfo.kind === "dict") {
        defaults[param.name] = "{}";
      } else {
        defaults[param.name] = "";
      }
    });

    if (Object.keys(defaults).length > 0) {
      setInputValues((prev) => ({ ...defaults, ...prev }));
    }
  }, [inputValues, parameters]);

  // Update parent when input values change
  useEffect(() => {
    if (onInputsChange) {
      onInputsChange(inputValues);
    }
  }, [inputValues, onInputsChange]);

  const handleInputChange = useCallback(
    (paramName: string, value: string) => {
      setInputValues((prev) => ({
        ...prev,
        [paramName]: value,
      }));

      if (onInputChange) {
        onInputChange(paramName, value);
      }
    },
    [onInputChange]
  );

  const errorDiagnostics = diagnostics.filter((d) => d.level === "error");

  return (
    <div className="schematic-sidebar">
      <div className="sidebar-content">
        <div className="module-header">
          <h2 className="module-name">{moduleName}</h2>
          <p className="module-path">{filePath}</p>
        </div>

        {/* Error Display */}
        {error && (
          <div className="error-alert">
            <AlertCircle className="error-icon" />
            <div className="error-message">{error}</div>
          </div>
        )}

        {/* Module Configuration - Only show config parameters */}
        {parameters.filter((p) => p.is_config).length > 0 && (
          <div className="configuration-section">
            <div className="section-header">
              <label className="section-label">Configuration</label>
              {isEvaluating && (
                <div className="evaluating-indicator">
                  <Loader className="spinner" size={12} />
                  Evaluating...
                </div>
              )}
            </div>

            <div className="parameters-list">
              {parameters
                .filter((param) => param.is_config)
                .map((param) => {
                  const typeInfo = param.type_info;
                  const isBoolean = typeInfo.kind === "bool";
                  const formattedType = formatTypeInfo(param);

                  return (
                    <div key={param.name} className="parameter-item">
                      <div className="parameter-header">
                        <label htmlFor={param.name} className="parameter-name">
                          {param.name}
                        </label>
                        <div className="parameter-type">{formattedType}</div>
                      </div>

                      {param.is_enum && param.enum_values ? (
                        <select
                          id={param.name}
                          className="parameter-select"
                          value={inputValues[param.name] || ""}
                          onChange={(e) =>
                            handleInputChange(param.name, e.target.value)
                          }
                        >
                          {param.enum_values.map((value) => (
                            <option key={value} value={value}>
                              {value}
                            </option>
                          ))}
                        </select>
                      ) : isBoolean ? (
                        <div className="parameter-checkbox-container">
                          <input
                            type="checkbox"
                            id={param.name}
                            className="parameter-checkbox"
                            checked={
                              inputValues[param.name] === "true" ||
                              inputValues[param.name] === "1"
                            }
                            onChange={(e) =>
                              handleInputChange(
                                param.name,
                                e.target.checked ? "true" : "false"
                              )
                            }
                          />
                          <label
                            htmlFor={param.name}
                            className="parameter-checkbox-label"
                          >
                            {inputValues[param.name] === "true"
                              ? "Enabled"
                              : "Disabled"}
                          </label>
                        </div>
                      ) : (
                        <input
                          id={param.name}
                          type="text"
                          className="parameter-input"
                          value={inputValues[param.name] || ""}
                          onChange={(e) =>
                            handleInputChange(param.name, e.target.value)
                          }
                          placeholder={
                            typeInfo.kind === "int" || typeInfo.kind === "float"
                              ? "e.g., 10k, 3.14"
                              : typeInfo.kind === "list"
                              ? "e.g., [1, 2, 3]"
                              : typeInfo.kind === "dict"
                              ? 'e.g., {"key": "value"}'
                              : `Enter ${param.name}`
                          }
                        />
                      )}
                    </div>
                  );
                })}
            </div>
          </div>
        )}

        {/* Diagnostics - Only show errors */}
        {errorDiagnostics.length > 0 && (
          <div className="diagnostics-section">
            <label className="section-label error-label">Errors</label>
            <div className="diagnostics-list">
              {errorDiagnostics.map((diag, idx) => (
                <div key={idx} className="diagnostic-item">
                  <div className="diagnostic-message">{diag.message}</div>
                  {diag.file && diag.line && (
                    <div className="diagnostic-location">
                      {diag.file}:{diag.line}
                    </div>
                  )}
                  {diag.child && (
                    <div className="diagnostic-child">
                      <div className="diagnostic-child-message">
                        {diag.child.message}
                      </div>
                      {diag.child.file && (
                        <div className="diagnostic-location">
                          {diag.child.file}:{diag.child.line || "?"}
                        </div>
                      )}
                    </div>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
};

export default SchematicSidebar;
