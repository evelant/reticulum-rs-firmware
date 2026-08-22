import { NomadPanel } from "../../components/NomadPanel.tsx";
import { useAppliance } from "../../lib/appliance-context.tsx";

export default function NomadScreen() {
  const {
    consumeNomadDestinationHint,
    nomadAvailable,
    nomadBrowser,
    nomadConnected,
    nomadDestinationHint,
    nomadState,
  } = useAppliance();

  return (
    <NomadPanel
      available={nomadAvailable}
      connected={nomadConnected}
      controller={nomadBrowser}
      destinationHint={nomadDestinationHint}
      onDestinationHintConsumed={consumeNomadDestinationHint}
      state={nomadState}
    />
  );
}
