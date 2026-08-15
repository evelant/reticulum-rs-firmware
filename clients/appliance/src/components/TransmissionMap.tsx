import {
  Camera,
  GeoJSONSource,
  Layer,
  Map as MapLibreNativeMap,
  type PressEventWithFeatures,
} from "@maplibre/maplibre-react-native";
import { useMemo, useRef } from "react";
import { StyleSheet } from "react-native";

import {
  selectedTransmissionMapFeatures,
  transmissionMapViewport,
} from "../lib/transmission-map.ts";
import {
  TRANSMISSION_MAP_LINE_HIT_LAYER,
  TRANSMISSION_MAP_LINE_LABEL_LAYER,
  TRANSMISSION_MAP_LINE_LAYER,
  TRANSMISSION_MAP_POINT_HIT_LAYER,
  TRANSMISSION_MAP_POINT_LABEL_LAYER,
  TRANSMISSION_MAP_POINT_LAYER,
  TRANSMISSION_MAP_RECEPTION_LINE_LAYER,
  TRANSMISSION_MAP_SELECTED_LINE_LAYER,
  TRANSMISSION_MAP_SELECTED_POINT_LAYER,
  TRANSMISSION_MAP_SOURCE_IDS,
  TRANSMISSION_MAP_STYLE_URL,
} from "../lib/transmission-map-style.ts";
import type { TransmissionMapProps } from "./TransmissionMap.types.ts";

function pressedFeatureIds(event: PressEventWithFeatures): string[] {
  return [
    ...new Set(
      event.features.flatMap((feature) => {
        const id = feature.properties?.id;
        return typeof id === "string" ? [id] : [];
      }),
    ),
  ];
}

export function TransmissionMap({
  onMapError,
  onMapReady,
  onSelectFeatures,
  scene,
  selectedFeatureId,
  viewportRevision,
}: TransmissionMapProps) {
  const selection = useMemo(
    () => selectedTransmissionMapFeatures(scene, selectedFeatureId),
    [scene, selectedFeatureId],
  );
  const nextViewport = useMemo(() => transmissionMapViewport(scene), [scene]);
  const hasPoints = scene.points.features.length > 0;
  const fittedViewport = useRef({
    hasPoints,
    revision: viewportRevision,
    viewport: nextViewport,
  });
  if (
    fittedViewport.current.revision !== viewportRevision ||
    (hasPoints && !fittedViewport.current.hasPoints)
  ) {
    fittedViewport.current = { hasPoints, revision: viewportRevision, viewport: nextViewport };
  } else {
    fittedViewport.current.hasPoints = hasPoints;
  }
  const viewport = fittedViewport.current.viewport;
  const onFeaturePress = (event: {
    nativeEvent: PressEventWithFeatures;
    stopPropagation(): void;
  }) => {
    event.stopPropagation();
    onSelectFeatures(pressedFeatureIds(event.nativeEvent));
  };

  return (
    <MapLibreNativeMap
      attribution
      logo={false}
      mapStyle={TRANSMISSION_MAP_STYLE_URL}
      onDidFailLoadingMap={() => onMapError?.("The basemap could not be loaded.")}
      onDidFinishLoadingMap={() => {
        onMapError?.(null);
        onMapReady?.();
      }}
      onPress={() => onSelectFeatures([])}
      style={styles.map}
    >
      <Camera
        center={[viewport.center[0], viewport.center[1]]}
        duration={450}
        initialViewState={{
          center: [viewport.center[0], viewport.center[1]],
          zoom: viewport.zoom,
        }}
        key={viewportRevision}
        zoom={viewport.zoom}
      />
      <GeoJSONSource
        data={scene.lines}
        hitbox={{ bottom: 22, left: 22, right: 22, top: 22 }}
        id={TRANSMISSION_MAP_SOURCE_IDS.lines}
        onPress={onFeaturePress}
      >
        <Layer {...TRANSMISSION_MAP_LINE_HIT_LAYER} />
        <Layer {...TRANSMISSION_MAP_LINE_LAYER} />
        <Layer {...TRANSMISSION_MAP_RECEPTION_LINE_LAYER} />
        <Layer {...TRANSMISSION_MAP_LINE_LABEL_LAYER} />
      </GeoJSONSource>
      <GeoJSONSource
        data={scene.points}
        hitbox={{ bottom: 22, left: 22, right: 22, top: 22 }}
        id={TRANSMISSION_MAP_SOURCE_IDS.points}
        onPress={onFeaturePress}
      >
        <Layer {...TRANSMISSION_MAP_POINT_HIT_LAYER} />
        <Layer {...TRANSMISSION_MAP_POINT_LAYER} />
        <Layer {...TRANSMISSION_MAP_POINT_LABEL_LAYER} />
      </GeoJSONSource>
      <GeoJSONSource data={selection} id={TRANSMISSION_MAP_SOURCE_IDS.selection}>
        <Layer {...TRANSMISSION_MAP_SELECTED_LINE_LAYER} />
        <Layer {...TRANSMISSION_MAP_SELECTED_POINT_LAYER} />
      </GeoJSONSource>
    </MapLibreNativeMap>
  );
}

const styles = StyleSheet.create({
  map: { flex: 1 },
});
