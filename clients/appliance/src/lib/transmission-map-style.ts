import type {
  CircleLayerSpecification,
  LineLayerSpecification,
  SymbolLayerSpecification,
} from "@maplibre/maplibre-react-native";
import type { ExpressionSpecification } from "maplibre-gl";

export const TRANSMISSION_MAP_STYLE_URL =
  process.env.EXPO_PUBLIC_MAP_STYLE_URL?.trim() || "https://tiles.openfreemap.org/styles/liberty";

export const TRANSMISSION_MAP_SOURCE_IDS = {
  lines: "reticulum-observation-lines",
  points: "reticulum-observation-points",
  selection: "reticulum-observation-selection",
} as const;

export const TRANSMISSION_MAP_LAYER_IDS = {
  line: "reticulum-observation-line",
  lineHit: "reticulum-observation-line-hit",
  lineLabel: "reticulum-observation-line-label",
  point: "reticulum-observation-point",
  pointHit: "reticulum-observation-point-hit",
  pointLabel: "reticulum-observation-point-label",
  receptionLine: "reticulum-message-reception-line",
  selectedLine: "reticulum-observation-selected-line",
  selectedPoint: "reticulum-observation-selected-point",
} as const;

const toneColor: ExpressionSpecification = [
  "match",
  ["get", "tone"],
  "success",
  "#50d890",
  "danger",
  "#ff6f61",
  "warning",
  "#e8c766",
  "info",
  "#62a9e8",
  "#8e9b91",
];

export const TRANSMISSION_MAP_LINE_HIT_LAYER: LineLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.lineHit,
  type: "line",
  source: TRANSMISSION_MAP_SOURCE_IDS.lines,
  paint: {
    "line-color": "#62a9e8",
    "line-opacity": 0.01,
    "line-width": 20,
  },
};

export const TRANSMISSION_MAP_LINE_LAYER: LineLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.line,
  type: "line",
  source: TRANSMISSION_MAP_SOURCE_IDS.lines,
  filter: ["==", ["get", "kind"], "observation-segment"],
  layout: {
    "line-cap": "round",
    "line-join": "round",
  },
  paint: {
    "line-color": "#62a9e8",
    "line-dasharray": [2, 2],
    "line-opacity": 0.72,
    "line-width": 3,
  },
};

export const TRANSMISSION_MAP_RECEPTION_LINE_LAYER: LineLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.receptionLine,
  type: "line",
  source: TRANSMISSION_MAP_SOURCE_IDS.lines,
  filter: ["==", ["get", "kind"], "message-reception-link"],
  layout: {
    "line-cap": "round",
    "line-join": "round",
  },
  paint: {
    "line-color": "#50d890",
    "line-opacity": 0.88,
    "line-width": 4,
  },
};

export const TRANSMISSION_MAP_LINE_LABEL_LAYER: SymbolLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.lineLabel,
  type: "symbol",
  source: TRANSMISSION_MAP_SOURCE_IDS.lines,
  layout: {
    "symbol-placement": "line-center",
    "text-field": ["get", "label"],
    "text-font": ["Noto Sans Regular"],
    "text-size": 11,
  },
  paint: {
    "text-color": "#dceaf5",
    "text-halo-color": "#101411",
    "text-halo-width": 2,
  },
};

export const TRANSMISSION_MAP_POINT_LAYER: CircleLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.point,
  type: "circle",
  source: TRANSMISSION_MAP_SOURCE_IDS.points,
  paint: {
    "circle-color": toneColor,
    "circle-opacity": ["case", ["==", ["get", "kind"], "message-location"], 0.32, 0.9],
    "circle-radius": [
      "case",
      ["==", ["get", "kind"], "message-location"],
      9,
      ["==", ["get", "kind"], "receiver-location"],
      8,
      7,
    ],
    "circle-stroke-color": toneColor,
    "circle-stroke-width": ["case", ["==", ["get", "kind"], "message-location"], 3, 1.5],
  },
};

export const TRANSMISSION_MAP_POINT_HIT_LAYER: CircleLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.pointHit,
  type: "circle",
  source: TRANSMISSION_MAP_SOURCE_IDS.points,
  paint: {
    "circle-color": "#62a9e8",
    "circle-opacity": 0.01,
    "circle-radius": 22,
  },
};

export const TRANSMISSION_MAP_POINT_LABEL_LAYER: SymbolLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.pointLabel,
  type: "symbol",
  source: TRANSMISSION_MAP_SOURCE_IDS.points,
  layout: {
    "text-anchor": "top",
    "text-field": ["get", "label"],
    "text-font": ["Noto Sans Regular"],
    "text-offset": [0, 1.05],
    "text-optional": true,
    "text-size": 11,
  },
  paint: {
    "text-color": "#f2f6f2",
    "text-halo-color": "#101411",
    "text-halo-width": 2,
  },
};

export const TRANSMISSION_MAP_SELECTED_LINE_LAYER: LineLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.selectedLine,
  type: "line",
  source: TRANSMISSION_MAP_SOURCE_IDS.selection,
  filter: ["==", ["geometry-type"], "LineString"],
  layout: {
    "line-cap": "round",
    "line-join": "round",
  },
  paint: {
    "line-color": "#ffffff",
    "line-opacity": 0.95,
    "line-width": 7,
  },
};

export const TRANSMISSION_MAP_SELECTED_POINT_LAYER: CircleLayerSpecification = {
  id: TRANSMISSION_MAP_LAYER_IDS.selectedPoint,
  type: "circle",
  source: TRANSMISSION_MAP_SOURCE_IDS.selection,
  filter: ["==", ["geometry-type"], "Point"],
  paint: {
    "circle-color": "#ffffff",
    "circle-opacity": 0.25,
    "circle-radius": 13,
    "circle-stroke-color": "#ffffff",
    "circle-stroke-width": 3,
  },
};

export const TRANSMISSION_MAP_WEB_LAYERS = [
  TRANSMISSION_MAP_LINE_HIT_LAYER,
  TRANSMISSION_MAP_LINE_LAYER,
  TRANSMISSION_MAP_RECEPTION_LINE_LAYER,
  TRANSMISSION_MAP_LINE_LABEL_LAYER,
  TRANSMISSION_MAP_POINT_HIT_LAYER,
  TRANSMISSION_MAP_POINT_LAYER,
  TRANSMISSION_MAP_POINT_LABEL_LAYER,
  TRANSMISSION_MAP_SELECTED_LINE_LAYER,
  TRANSMISSION_MAP_SELECTED_POINT_LAYER,
];
