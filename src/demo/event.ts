import type { EventCallback, EventName, Options, UnlistenFn } from '@tauri-apps/api/event';

// The screenshot represents an idle application. No real IPC or events are used.
export async function listen<T>(_event: EventName, _handler: EventCallback<T>, _options?: Options): Promise<UnlistenFn> {
  return () => {};
}
