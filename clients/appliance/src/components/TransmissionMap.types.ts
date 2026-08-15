import type { TransmissionMapScene } from "../lib/transmission-map.ts";

export interface TransmissionMapProps {
  readonly onMapError?: (message: string | null) => void;
  readonly onMapReady?: () => void;
  readonly onSelectFeatures: (featureIds: readonly string[]) => void;
  readonly scene: TransmissionMapScene;
  readonly selectedFeatureId: string | null;
  readonly viewportRevision: number;
}
