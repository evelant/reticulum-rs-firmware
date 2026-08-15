import maplibregl, {
  type GeoJSONSource,
  type Map as MapLibreMap,
  type MapMouseEvent,
} from "maplibre-gl";
import { useEffect, useMemo, useRef } from "react";
import "maplibre-gl/dist/maplibre-gl.css";

import {
  selectedTransmissionMapFeatures,
  transmissionMapViewport,
} from "../lib/transmission-map.ts";
import {
  TRANSMISSION_MAP_LAYER_IDS,
  TRANSMISSION_MAP_SOURCE_IDS,
  TRANSMISSION_MAP_STYLE_URL,
  TRANSMISSION_MAP_WEB_LAYERS,
} from "../lib/transmission-map-style.ts";
import type { TransmissionMapProps } from "./TransmissionMap.types.ts";

const INTERACTIVE_LAYER_IDS = [
  TRANSMISSION_MAP_LAYER_IDS.lineHit,
  TRANSMISSION_MAP_LAYER_IDS.lineLabel,
  TRANSMISSION_MAP_LAYER_IDS.pointHit,
  TRANSMISSION_MAP_LAYER_IDS.point,
  TRANSMISSION_MAP_LAYER_IDS.pointLabel,
];

function featureIds(features: readonly { readonly properties?: unknown }[]): string[] {
  return [
    ...new Set(
      features.flatMap((feature) => {
        const properties = feature.properties;
        if (properties === null || typeof properties !== "object") return [];
        const id = (properties as { readonly id?: unknown }).id;
        return typeof id === "string" ? [id] : [];
      }),
    ),
  ];
}

function updateGeoJsonSource(map: MapLibreMap, id: string, data: GeoJSON.FeatureCollection) {
  const source = map.getSource(id);
  if (source !== undefined) (source as GeoJSONSource).setData(data);
}

export function TransmissionMap({
  onMapError,
  onMapReady,
  onSelectFeatures,
  scene,
  selectedFeatureId,
  viewportRevision,
}: TransmissionMapProps) {
  const container = useRef<HTMLElement>(null);
  const map = useRef<MapLibreMap | null>(null);
  const sceneRef = useRef(scene);
  const selectRef = useRef(onSelectFeatures);
  const errorRef = useRef(onMapError);
  const mapReadyRef = useRef(onMapReady);
  const selectedFeatureIdRef = useRef(selectedFeatureId);
  const ready = useRef(false);
  const hadPoints = useRef(scene.points.features.length > 0);
  const viewport = useMemo(() => transmissionMapViewport(scene), [scene]);
  const viewportRef = useRef(viewport);
  const selection = useMemo(
    () => selectedTransmissionMapFeatures(scene, selectedFeatureId),
    [scene, selectedFeatureId],
  );
  sceneRef.current = scene;
  selectRef.current = onSelectFeatures;
  errorRef.current = onMapError;
  mapReadyRef.current = onMapReady;
  selectedFeatureIdRef.current = selectedFeatureId;
  viewportRef.current = viewport;

  useEffect(() => {
    if (container.current === null) return;
    const initial = transmissionMapViewport(sceneRef.current);
    const instance = new maplibregl.Map({
      attributionControl: false,
      center: [initial.center[0], initial.center[1]],
      container: container.current,
      style: TRANSMISSION_MAP_STYLE_URL,
      zoom: initial.zoom,
    });
    map.current = instance;
    instance.addControl(new maplibregl.NavigationControl({ showCompass: false }), "top-right");
    instance.addControl(new maplibregl.AttributionControl({ compact: true }), "bottom-right");
    let lastMapErrorAt: number | null = null;
    let clearMapErrorTimer: number | null = null;

    const pointAtFeature = () => {
      instance.getCanvas().style.cursor = "pointer";
    };
    const leaveFeature = () => {
      instance.getCanvas().style.cursor = "";
    };
    const selectAtPoint = (event: MapMouseEvent) => {
      selectRef.current(
        featureIds(instance.queryRenderedFeatures(event.point, { layers: INTERACTIVE_LAYER_IDS })),
      );
    };

    instance.on("load", () => {
      const current = sceneRef.current;
      instance.addSource(TRANSMISSION_MAP_SOURCE_IDS.lines, {
        type: "geojson",
        data: current.lines,
      });
      instance.addSource(TRANSMISSION_MAP_SOURCE_IDS.points, {
        type: "geojson",
        data: current.points,
      });
      instance.addSource(TRANSMISSION_MAP_SOURCE_IDS.selection, {
        type: "geojson",
        data: selectedTransmissionMapFeatures(current, selectedFeatureIdRef.current),
      });
      for (const layer of TRANSMISSION_MAP_WEB_LAYERS) instance.addLayer(layer);
      for (const layerId of INTERACTIVE_LAYER_IDS) {
        instance.on("mouseenter", layerId, pointAtFeature);
        instance.on("mouseleave", layerId, leaveFeature);
      }
      instance.on("click", selectAtPoint);
      const currentViewport = viewportRef.current;
      instance.jumpTo({
        center: [currentViewport.center[0], currentViewport.center[1]],
        zoom: currentViewport.zoom,
      });
      const currentHasPoints = current.points.features.length > 0;
      hadPoints.current = currentHasPoints;
      ready.current = true;
      lastMapErrorAt = null;
      errorRef.current?.(null);
      mapReadyRef.current?.();
    });
    instance.on("error", (event) => {
      lastMapErrorAt = Date.now();
      if (clearMapErrorTimer !== null) window.clearTimeout(clearMapErrorTimer);
      errorRef.current?.(event.error?.message ?? "The basemap could not be loaded.");
    });
    instance.on("idle", () => {
      if (clearMapErrorTimer !== null) window.clearTimeout(clearMapErrorTimer);
      const visibleFor = lastMapErrorAt === null ? 0 : Date.now() - lastMapErrorAt;
      clearMapErrorTimer = window.setTimeout(
        () => {
          lastMapErrorAt = null;
          clearMapErrorTimer = null;
          errorRef.current?.(null);
        },
        Math.max(0, 2_000 - visibleFor),
      );
    });

    return () => {
      if (clearMapErrorTimer !== null) window.clearTimeout(clearMapErrorTimer);
      ready.current = false;
      map.current = null;
      instance.remove();
    };
  }, []);

  useEffect(() => {
    const instance = map.current;
    if (instance === null || !ready.current) return;
    updateGeoJsonSource(instance, TRANSMISSION_MAP_SOURCE_IDS.points, scene.points);
    updateGeoJsonSource(instance, TRANSMISSION_MAP_SOURCE_IDS.lines, scene.lines);
    const currentHasPoints = scene.points.features.length > 0;
    if (!hadPoints.current && currentHasPoints) {
      instance.easeTo({
        center: [viewport.center[0], viewport.center[1]],
        duration: 450,
        zoom: viewport.zoom,
      });
    }
    hadPoints.current = currentHasPoints;
  }, [scene.lines, scene.points, viewport.center, viewport.zoom]);

  useEffect(() => {
    // The Fit action increments this revision to re-run the current-scene camera transition.
    void viewportRevision;
    const instance = map.current;
    if (instance === null || !ready.current) return;
    const nextViewport = viewportRef.current;
    instance.easeTo({
      center: [nextViewport.center[0], nextViewport.center[1]],
      duration: 450,
      zoom: nextViewport.zoom,
    });
  }, [viewportRevision]);

  useEffect(() => {
    const instance = map.current;
    if (instance === null || !ready.current) return;
    updateGeoJsonSource(instance, TRANSMISSION_MAP_SOURCE_IDS.selection, selection);
  }, [selection]);

  return (
    <section
      aria-label="Transmission locations map"
      ref={container}
      style={{ height: "100%", minHeight: 0, width: "100%" }}
    />
  );
}
