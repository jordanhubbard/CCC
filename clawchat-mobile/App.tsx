import React, { useEffect, useState } from 'react';
import { NavigationContainer } from '@react-navigation/native';
import { createNativeStackNavigator } from '@react-navigation/native-stack';
import { ActivityIndicator, View, StyleSheet } from 'react-native';
import { getToken } from './src/store/auth';
import AuthScreen from './src/screens/AuthScreen';
import ChannelListScreen from './src/screens/ChannelListScreen';
import MessagesScreen from './src/screens/MessagesScreen';

export type RootStackParamList = {
  Auth: undefined;
  ChannelList: undefined;
  Messages: {
    channelId: string;
    channelName: string;
    isDM?: boolean;
  };
};

const Stack = createNativeStackNavigator<RootStackParamList>();

export default function App() {
  const [initialRoute, setInitialRoute] = useState<keyof RootStackParamList | null>(null);

  useEffect(() => {
    getToken().then((token) => {
      setInitialRoute(token ? 'ChannelList' : 'Auth');
    });
  }, []);

  if (!initialRoute) {
    return (
      <View style={styles.splash}>
        <ActivityIndicator color="#e94560" size="large" />
      </View>
    );
  }

  return (
    <NavigationContainer>
      <Stack.Navigator
        initialRouteName={initialRoute}
        screenOptions={{
          headerStyle: { backgroundColor: '#16213e' },
          headerTintColor: '#fff',
          headerTitleStyle: { fontWeight: '700' },
          headerBackTitleVisible: false,
          contentStyle: { backgroundColor: '#1a1a2e' },
        }}>
        <Stack.Screen
          name="Auth"
          component={AuthScreen}
          options={{ headerShown: false }}
        />
        <Stack.Screen
          name="ChannelList"
          component={ChannelListScreen}
          options={{ headerShown: false }}
        />
        <Stack.Screen
          name="Messages"
          component={MessagesScreen}
          options={({ route }) => ({ title: `#${route.params.channelName}` })}
        />
      </Stack.Navigator>
    </NavigationContainer>
  );
}

const styles = StyleSheet.create({
  splash: {
    flex: 1,
    justifyContent: 'center',
    alignItems: 'center',
    backgroundColor: '#1a1a2e',
  },
});
