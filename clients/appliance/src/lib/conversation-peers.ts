export interface ConversationPeerSummary {
  readonly destination: string;
  readonly inbound_message_count: number;
  readonly message_count: number;
  readonly name: string | null;
}

/**
 * Peers with durable message history that the user has not promoted into the
 * local address book. Authentication and contact trust remain distinct.
 */
export function messageRequestPeers<T extends ConversationPeerSummary>(
  peers: readonly T[],
): readonly T[] {
  return peers.filter((peer) => peer.name === null && peer.inbound_message_count > 0);
}

export function outboundOnlyUnsavedPeers<T extends ConversationPeerSummary>(
  peers: readonly T[],
): readonly T[] {
  return peers.filter(
    (peer) => peer.name === null && peer.inbound_message_count === 0 && peer.message_count > 0,
  );
}

export function conversationPeerLabel(peer: Pick<ConversationPeerSummary, "destination" | "name">) {
  if (peer.name !== null && peer.name.trim().length > 0) return peer.name;
  return `Unknown …${peer.destination.slice(-6)}`;
}

export function suggestedContactName(destination: string): string {
  return `Peer ${destination.slice(-6)}`;
}
