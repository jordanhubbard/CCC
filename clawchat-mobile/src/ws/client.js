import AsyncStorage from '@react-native-async-storage/async-storage';

const WS_BASE = 'ws://146.190.134.110:8793/ws';
const RECONNECT_DELAY_MS = 3000;
const MAX_RECONNECT_ATTEMPTS = 10;

class WSClient {
  constructor() {
    this.ws = null;
    this.listeners = {};
    this.reconnectAttempts = 0;
    this.shouldReconnect = false;
    this.channelId = null;
  }

  async connect(channelId) {
    this.channelId = channelId;
    this.shouldReconnect = true;
    this.reconnectAttempts = 0;
    await this._open();
  }

  async _open() {
    const token = await AsyncStorage.getItem('auth_token');
    if (!token) {
      console.warn('[WS] No token available, skipping connect');
      return;
    }

    const url = `${WS_BASE}?token=${encodeURIComponent(token)}`;
    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      console.log('[WS] Connected');
      this.reconnectAttempts = 0;
      this._emit('open');
    };

    this.ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        this._emit('message', data);
      } catch (e) {
        console.warn('[WS] Failed to parse message', event.data);
      }
    };

    this.ws.onerror = (err) => {
      console.warn('[WS] Error', err.message);
      this._emit('error', err);
    };

    this.ws.onclose = () => {
      console.log('[WS] Closed');
      this._emit('close');
      if (this.shouldReconnect && this.reconnectAttempts < MAX_RECONNECT_ATTEMPTS) {
        this.reconnectAttempts++;
        console.log(`[WS] Reconnecting in ${RECONNECT_DELAY_MS}ms (attempt ${this.reconnectAttempts})`);
        setTimeout(() => this._open(), RECONNECT_DELAY_MS);
      }
    };
  }

  send(data) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(data));
    }
  }

  disconnect() {
    this.shouldReconnect = false;
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  on(event, callback) {
    if (!this.listeners[event]) {
      this.listeners[event] = [];
    }
    this.listeners[event].push(callback);
    return () => this.off(event, callback);
  }

  off(event, callback) {
    if (this.listeners[event]) {
      this.listeners[event] = this.listeners[event].filter((cb) => cb !== callback);
    }
  }

  _emit(event, data) {
    (this.listeners[event] || []).forEach((cb) => cb(data));
  }
}

// Singleton instance shared across the app
export const wsClient = new WSClient();
export default wsClient;
