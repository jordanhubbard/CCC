import React from 'react';
import { View, StyleSheet } from 'react-native';

interface Props {
  status: string;
  size?: number;
}

export default function PresenceDot({ status, size = 10 }: Props) {
  const color = status === 'online' ? '#4caf50' : '#9e9e9e';
  return (
    <View
      style={[
        styles.dot,
        { width: size, height: size, borderRadius: size / 2, backgroundColor: color },
      ]}
    />
  );
}

const styles = StyleSheet.create({
  dot: {
    borderWidth: 1.5,
    borderColor: '#1a1a2e',
  },
});
