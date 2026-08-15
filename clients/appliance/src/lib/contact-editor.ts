export interface ContactSaveIntent {
  readonly destination: string;
  readonly name: string;
  readonly selectAfterSave: boolean;
}

/**
 * Resolve one contact-form save without allowing an edit to retarget the
 * contact or change the active conversation.
 */
export function contactSaveIntent(
  name: string,
  destinationInput: string,
  editingDestination: string | null,
): ContactSaveIntent {
  const editing = editingDestination !== null;
  return {
    destination: editing ? editingDestination : destinationInput.trim().toLowerCase(),
    name,
    selectAfterSave: !editing,
  };
}
