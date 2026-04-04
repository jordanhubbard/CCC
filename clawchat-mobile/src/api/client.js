import axios from 'axios';
import AsyncStorage from '@react-native-async-storage/async-storage';

const BASE_URL = 'http://146.190.134.110:8793/api';

const apiClient = axios.create({
  baseURL: BASE_URL,
  timeout: 10000,
  headers: {
    'Content-Type': 'application/json',
  },
});

// Attach JWT token to every request
apiClient.interceptors.request.use(async (config) => {
  const token = await AsyncStorage.getItem('auth_token');
  if (token) {
    config.headers.Authorization = `Bearer ${token}`;
  }
  return config;
});

// Auth
export const login = (username, password) =>
  apiClient.post('/auth/login', { username, password }).then((r) => r.data);

// Channels
export const getChannels = () =>
  apiClient.get('/channels').then((r) => r.data);

// Messages
export const getMessages = (channelId, limit = 50) =>
  apiClient
    .get('/messages', { params: { channel_id: channelId, limit } })
    .then((r) => r.data);

export const postMessage = (channelId, content) =>
  apiClient.post('/messages', { channel_id: channelId, content }).then((r) => r.data);

// Presence
export const getPresence = () =>
  apiClient.get('/presence').then((r) => r.data);

// DMs
export const getDMs = () =>
  apiClient.get('/dms').then((r) => r.data);

export default apiClient;
