import { useEffect, useState, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface DebugState {
  hotkeys: Record<string, string>;
  listener_alive: boolean;
  is_recording: boolean;
  timestamp: string;
}

interface HotkeyEvent {
  name: string;
  timestamp: string;
}

// Maps the short action key (used by simulate buttons) to the full event name
// that the backend emits and the frontend listens for.
const ACTION_TO_EVENT: Record<string, string> = {
  "start":            "hotkey-start-recording",
  "stop":             "hotkey-stop-recording",
  "pause":            "hotkey-pause-recording",
  "library":          "hotkey-open-library",
  "replay_toggle":    "hotkey-replay-toggle",
  "replay_save":      "hotkey-replay-save",
};

interface Props {
  enabled: boolean;
}

export function DebugPanel({ enabled }: Props) {
  const [debugState, setDebugState] = useState<DebugState | null>(null);
  const [hotkeyEvents, setHotkeyEvents] = useState<HotkeyEvent[]>([]);
  // Start collapsed to avoid hiding content
  const [isExpanded, setIsExpanded] = useState(false);
  const eventListRef = useRef<HTMLDivElement>(null);

  // Don't render at all if disabled
  if (!enabled) {
    return null;
  }

  // Poll backend debug state every 1 second (runs even when collapsed)
  useEffect(() => {
    const pollState = async () => {
      try {
        const state = await invoke<DebugState>("get_debug_state");
        setDebugState(state);
      } catch (e) {
        console.error("Failed to get debug state:", e);
      }
    };

    pollState();
    const interval = setInterval(pollState, 1000);
    return () => clearInterval(interval);
  }, []);

  // Listen for all hotkey events so the debug panel shows them arriving (runs even when collapsed)
  useEffect(() => {
    const eventNames = Object.values(ACTION_TO_EVENT);
    const unlisteners = eventNames.map(name =>
      listen(name, () => {
        const ts = new Date().toLocaleTimeString();
        console.log(`[DebugPanel] Event received: ${name} at ${ts}`);
        setHotkeyEvents(prev => [{ name, timestamp: ts }, ...prev].slice(0, 50));
      })
    );
    return () => { unlisteners.forEach(p => p.then(f => f())); };
  }, []);

  // Auto-scroll to latest event
  useEffect(() => {
    if (eventListRef.current) {
      eventListRef.current.scrollTop = 0;
    }
  }, [hotkeyEvents]);

  // action is the short key e.g. "start-recording" → simulate_hotkey gets "start-recording"
  // Rust: format!("hotkey-{action}") → "hotkey-start-recording" ✓
  const simulateHotkey = async (action: string) => {
    try {
      console.log(`[DebugPanel] Simulating: ${action}`);
      await invoke("simulate_hotkey", { action });
    } catch (e) {
      console.error(`Failed to simulate hotkey ${action}:`, e);
    }
  };

  // If not expanded, show just the button
  if (!isExpanded) {
    return (
      <button
        onClick={() => setIsExpanded(true)}
        style={{
          position: "fixed",
          bottom: 20,
          right: 20,
          padding: "8px 12px",
          background: "rgba(100, 100, 255, 0.9)",
          color: "#fff",
          border: "none",
          borderRadius: 4,
          cursor: "pointer",
          fontSize: 12,
          fontWeight: 600,
          zIndex: 1000,
          boxShadow: "0 2px 8px rgba(0,0,0,0.3)",
        }}
        title="Click to open debug panel"
      >
        🐛 Debug
      </button>
    );
  }

  return (
    <div
      style={{
        position: "fixed",
        bottom: 20,
        right: 20,
        width: 500,
        maxHeight: 600,
        background: "rgba(20, 20, 40, 0.95)",
        border: "1px solid rgba(100, 100, 255, 0.5)",
        borderRadius: 8,
        padding: 12,
        color: "#fff",
        fontSize: 11,
        fontFamily: "monospace",
        zIndex: 1000,
        display: "flex",
        flexDirection: "column",
        boxShadow: "0 4px 20px rgba(0,0,0,0.5)",
      }}
    >
      {/* Header */}
      <div
        style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          marginBottom: 10,
          paddingBottom: 8,
          borderBottom: "1px solid rgba(100, 100, 255, 0.3)",
        }}
      >
        <span style={{ fontWeight: 600, color: "#6495ED" }}>🐛 Debug Panel</span>
        <button
          onClick={() => setIsExpanded(false)}
          style={{
            background: "transparent",
            border: "none",
            color: "#fff",
            cursor: "pointer",
            fontSize: 14,
          }}
        >
          ✕
        </button>
      </div>

      {/* State Info */}
      {debugState && (
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: 8,
            marginBottom: 10,
            fontSize: 10,
          }}
        >
          <div>
            <span style={{ color: "#90EE90" }}>Listener:</span>{" "}
            <span style={{ color: debugState.listener_alive ? "#00FF00" : "#FF6B6B" }}>
              {debugState.listener_alive ? "✓ ALIVE" : "✗ DEAD"}
            </span>
          </div>
          <div>
            <span style={{ color: "#90EE90" }}>Recording:</span>{" "}
            <span style={{ color: debugState.is_recording ? "#FF6B6B" : "#888" }}>
              {debugState.is_recording ? "✓ ON" : "○ OFF"}
            </span>
          </div>
          <div style={{ gridColumn: "1 / -1" }}>
            <span style={{ color: "#90EE90" }}>Time:</span> {debugState.timestamp}
          </div>
        </div>
      )}

      {/* Hotkey Bindings */}
      <div style={{ marginBottom: 10, fontSize: 9 }}>
        <div style={{ color: "#FFD700", marginBottom: 4, fontWeight: 600 }}>Hotkeys:</div>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "1fr 1fr",
            gap: 4,
            background: "rgba(0,0,0,0.3)",
            padding: 6,
            borderRadius: 4,
            maxHeight: 80,
            overflowY: "auto",
          }}
        >
          {debugState?.hotkeys &&
            Object.entries(debugState.hotkeys).map(([action, key]) => (
              <div key={action}>
                <span style={{ color: "#87CEEB" }}>{action}:</span>{" "}
                <span style={{ color: "#FFB6C1" }}>{key}</span>
              </div>
            ))}
        </div>
      </div>

      {/* Simulate Hotkey Buttons */}
      <div style={{ marginBottom: 10 }}>
        <div style={{ color: "#FFD700", marginBottom: 4, fontSize: 9, fontWeight: 600 }}>
          Simulate:
        </div>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: "repeat(3, 1fr)",
            gap: 4,
          }}
        >
          {Object.keys(ACTION_TO_EVENT).map((action) => (
            <button
              key={action}
              onClick={() => simulateHotkey(action)}
              style={{
                padding: "4px 6px",
                background: "rgba(100, 100, 255, 0.6)",
                border: "1px solid rgba(100, 100, 255, 0.8)",
                color: "#fff",
                borderRadius: 3,
                cursor: "pointer",
                fontSize: 9,
                fontFamily: "monospace",
                fontWeight: 500,
              }}
              title={`Simulate ${action} hotkey`}
            >
              {action.replace("_", " ")}
            </button>
          ))}
        </div>
      </div>

      {/* Hotkey Events (Last 10) */}
      <div style={{ flex: 1, display: "flex", flexDirection: "column" }}>
        <div style={{ color: "#FFD700", marginBottom: 4, fontSize: 9, fontWeight: 600 }}>
          Recent Events (last 50):
        </div>
        <div
          ref={eventListRef}
          style={{
            flex: 1,
            background: "rgba(0,0,0,0.4)",
            borderRadius: 4,
            padding: 6,
            overflowY: "auto",
            fontSize: 8,
          }}
        >
          {hotkeyEvents.length === 0 ? (
            <div style={{ color: "#888", fontStyle: "italic" }}>No events yet...</div>
          ) : (
            hotkeyEvents.map((evt, i) => (
              <div key={i} style={{ color: "#90EE90", marginBottom: 2 }}>
                <span style={{ color: "#FFD700" }}>[{evt.timestamp}]</span> {evt.name}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
