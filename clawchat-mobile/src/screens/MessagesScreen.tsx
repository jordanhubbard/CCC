import React, { useEffect, useState, useRef, useCallback } from 'react';
import {
  View,
  Text,
  FlatList,
  TextInput,
  TouchableOpacity,
  StyleSheet,
  ActivityIndicator,
  KeyboardAvoidingView,
  Platform,
  Alert,
} from 'react-native';
import { NativeStackNavigationProp } from '@react-navigation/native-stack';
import { RouteProp } from '@react-navigation/native';
import { getMessages, postMessage } from '../api/client';
import { getUser } from '../store/auth';
import wsClient from '../ws/client';
import MessageBubble from '../components/MessageBubble';
import { RootStackParamList } from '../../App';

type Props = {
  navigation: NativeStackNavigationProp<RootStackParamList, 'Messages'>;
  route: RouteProp<RootStackParamList, 'Messages'>;
};

interface Message {
  id: string | number;
  content: string;
  author: string;
  ts: string;
}

export default function MessagesScreen({ navigation, route }: Props) {
  const { channelId, channelName } = route.params;
  const [messages, setMessages] = useState<Message[]>([]);
  const [loading, setLoading] = useState(true);
  const [sending, setSending] = useState(false);
  const [draft, setDraft] = useState('');
  const [currentUser, setCurrentUser] = useState<string>('');
  const flatListRef = useRef<FlatList>(null);

  // Load initial messages
  const fetchMessages = useCallback(async () => {
    try {
      const data = await getMessages(channelId);
      setMessages(Array.isArray(data) ? data : []);
    } catch (err: any) {
      Alert.alert('Error', err?.message || 'Failed to load messages');
    } finally {
      setLoading(false);
    }
  }, [channelId]);

  useEffect(() => {
    navigation.setOptions({ title: `#${channelName}` });

    // Get current user for "isOwn" bubble coloring
    getUser().then((u) => {
      if (u?.username) setCurrentUser(u.username);
    });

    fetchMessages();

    // Connect WebSocket and listen for new messages
    wsClient.connect(channelId);
    const unsubscribe = wsClient.on('message', (data: any) => {
      // Filter to only messages for this channel
      if (data?.channel_id === channelId || data?.channel_id === Number(channelId)) {
        const msg: Message = {
          id: data.id ?? Date.now(),
          content: data.content,
          author: data.author,
          ts: data.ts ?? new Date().toISOString(),
        };
        setMessages((prev) => {
          // Deduplicate by id
          if (prev.some((m) => m.id === msg.id)) return prev;
          return [...prev, msg];
        });
      }
    });

    return () => {
      unsubscribe();
      wsClient.disconnect();
    };
  }, [channelId, channelName, fetchMessages, navigation]);

  // Scroll to bottom when messages change
  useEffect(() => {
    if (messages.length > 0) {
      setTimeout(() => flatListRef.current?.scrollToEnd({ animated: true }), 100);
    }
  }, [messages]);

  const handleSend = async () => {
    const content = draft.trim();
    if (!content) return;
    setDraft('');
    setSending(true);
    try {
      await postMessage(channelId, content);
      // Optimistic: if WS doesn't echo back, re-fetch
      const data = await getMessages(channelId);
      setMessages(Array.isArray(data) ? data : []);
    } catch (err: any) {
      Alert.alert('Error', err?.message || 'Failed to send message');
      setDraft(content); // restore draft on failure
    } finally {
      setSending(false);
    }
  };

  return (
    <KeyboardAvoidingView
      style={styles.container}
      behavior={Platform.OS === 'ios' ? 'padding' : undefined}
      keyboardVerticalOffset={90}>
      {loading ? (
        <ActivityIndicator style={styles.loader} color="#e94560" size="large" />
      ) : (
        <FlatList
          ref={flatListRef}
          data={messages}
          keyExtractor={(item) => String(item.id)}
          renderItem={({ item }) => (
            <MessageBubble message={item} isOwn={item.author === currentUser} />
          )}
          contentContainerStyle={styles.messageList}
          ListEmptyComponent={
            <Text style={styles.empty}>No messages yet. Say hello!</Text>
          }
        />
      )}

      {/* Composer */}
      <View style={styles.composer}>
        <TextInput
          style={styles.input}
          placeholder={`Message #${channelName}`}
          placeholderTextColor="#888"
          value={draft}
          onChangeText={setDraft}
          multiline
          maxLength={2000}
        />
        <TouchableOpacity
          style={[styles.sendBtn, (!draft.trim() || sending) && styles.sendBtnDisabled]}
          onPress={handleSend}
          disabled={!draft.trim() || sending}>
          {sending ? (
            <ActivityIndicator color="#fff" size="small" />
          ) : (
            <Text style={styles.sendText}>Send</Text>
          )}
        </TouchableOpacity>
      </View>
    </KeyboardAvoidingView>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, backgroundColor: '#1a1a2e' },
  loader: { marginTop: 60 },
  messageList: { paddingVertical: 12, flexGrow: 1 },
  empty: { textAlign: 'center', color: '#888', marginTop: 60, fontSize: 15 },
  composer: {
    flexDirection: 'row',
    alignItems: 'flex-end',
    paddingHorizontal: 12,
    paddingVertical: 8,
    borderTopWidth: 1,
    borderTopColor: '#0f3460',
    backgroundColor: '#16213e',
  },
  input: {
    flex: 1,
    backgroundColor: '#1a1a2e',
    borderRadius: 20,
    paddingHorizontal: 16,
    paddingVertical: 10,
    fontSize: 15,
    color: '#fff',
    maxHeight: 120,
    borderWidth: 1,
    borderColor: '#0f3460',
  },
  sendBtn: {
    marginLeft: 10,
    backgroundColor: '#e94560',
    borderRadius: 20,
    paddingHorizontal: 18,
    paddingVertical: 10,
    alignItems: 'center',
    justifyContent: 'center',
    minWidth: 64,
  },
  sendBtnDisabled: { opacity: 0.5 },
  sendText: { color: '#fff', fontWeight: '700', fontSize: 15 },
});
