import React from 'react';
import { View, Text, StyleSheet } from 'react-native';

interface Message {
  id: string | number;
  content: string;
  author: string;
  ts: string;
}

interface Props {
  message: Message;
  isOwn: boolean;
}

export default function MessageBubble({ message, isOwn }: Props) {
  const time = new Date(message.ts).toLocaleTimeString([], {
    hour: '2-digit',
    minute: '2-digit',
  });

  return (
    <View style={[styles.wrapper, isOwn && styles.wrapperOwn]}>
      {!isOwn && <Text style={styles.author}>{message.author}</Text>}
      <View style={[styles.bubble, isOwn ? styles.bubbleOwn : styles.bubbleOther]}>
        <Text style={styles.content}>{message.content}</Text>
        <Text style={styles.time}>{time}</Text>
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  wrapper: {
    marginVertical: 4,
    marginHorizontal: 12,
    alignItems: 'flex-start',
    maxWidth: '80%',
  },
  wrapperOwn: {
    alignSelf: 'flex-end',
    alignItems: 'flex-end',
  },
  author: {
    fontSize: 12,
    color: '#888',
    marginBottom: 2,
    marginLeft: 4,
  },
  bubble: {
    borderRadius: 16,
    paddingHorizontal: 14,
    paddingVertical: 8,
  },
  bubbleOther: {
    backgroundColor: '#16213e',
    borderBottomLeftRadius: 4,
  },
  bubbleOwn: {
    backgroundColor: '#e94560',
    borderBottomRightRadius: 4,
  },
  content: {
    color: '#fff',
    fontSize: 15,
    lineHeight: 20,
  },
  time: {
    color: 'rgba(255,255,255,0.6)',
    fontSize: 11,
    marginTop: 4,
    alignSelf: 'flex-end',
  },
});
