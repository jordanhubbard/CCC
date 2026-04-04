import React, { useEffect, useState, useCallback } from 'react';
import {
  View,
  Text,
  FlatList,
  TouchableOpacity,
  StyleSheet,
  ActivityIndicator,
  RefreshControl,
  Alert,
} from 'react-native';
import { NativeStackNavigationProp } from '@react-navigation/native-stack';
import { getChannels, getPresence, getDMs } from '../api/client';
import { clearAuth } from '../store/auth';
import PresenceDot from '../components/PresenceDot';
import { RootStackParamList } from '../../App';

type Props = {
  navigation: NativeStackNavigationProp<RootStackParamList, 'ChannelList'>;
};

interface Channel {
  id: string | number;
  name: string;
  description?: string;
}

interface PresenceEntry {
  agent: string;
  status: string;
}

type Tab = 'channels' | 'dms';

export default function ChannelListScreen({ navigation }: Props) {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [dms, setDMs] = useState<Channel[]>([]);
  const [presence, setPresence] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [activeTab, setActiveTab] = useState<Tab>('channels');

  const fetchData = useCallback(async () => {
    try {
      const [channelData, presenceData, dmData] = await Promise.allSettled([
        getChannels(),
        getPresence(),
        getDMs(),
      ]);

      if (channelData.status === 'fulfilled') {
        setChannels(channelData.value);
      }
      if (presenceData.status === 'fulfilled') {
        const map: Record<string, string> = {};
        (presenceData.value as PresenceEntry[]).forEach((p) => {
          map[p.agent] = p.status;
        });
        setPresence(map);
      }
      if (dmData.status === 'fulfilled') {
        setDMs(dmData.value);
      }
    } catch (err: any) {
      Alert.alert('Error', err?.message || 'Failed to load channels');
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    fetchData();
    // Refresh presence every 30s
    const interval = setInterval(() => {
      getPresence()
        .then((data: PresenceEntry[]) => {
          const map: Record<string, string> = {};
          data.forEach((p) => { map[p.agent] = p.status; });
          setPresence(map);
        })
        .catch(() => {});
    }, 30000);
    return () => clearInterval(interval);
  }, [fetchData]);

  const handleLogout = async () => {
    await clearAuth();
    navigation.replace('Auth');
  };

  const openChannel = (channel: Channel, isDM = false) => {
    navigation.navigate('Messages', {
      channelId: String(channel.id),
      channelName: channel.name,
      isDM,
    });
  };

  const renderChannel = ({ item }: { item: Channel }) => {
    const status = presence[item.name] ?? 'offline';
    return (
      <TouchableOpacity
        style={styles.row}
        onPress={() => openChannel(item, activeTab === 'dms')}>
        <View style={styles.rowLeft}>
          <Text style={styles.hash}>{activeTab === 'dms' ? '@' : '#'}</Text>
          <View>
            <Text style={styles.channelName}>{item.name}</Text>
            {item.description ? (
              <Text style={styles.description} numberOfLines={1}>
                {item.description}
              </Text>
            ) : null}
          </View>
        </View>
        {activeTab === 'dms' && (
          <PresenceDot status={status} size={10} />
        )}
      </TouchableOpacity>
    );
  };

  const list = activeTab === 'channels' ? channels : dms;

  return (
    <View style={styles.container}>
      {/* Header */}
      <View style={styles.header}>
        <Text style={styles.headerTitle}>ClawChat</Text>
        <TouchableOpacity onPress={handleLogout}>
          <Text style={styles.logoutText}>Sign Out</Text>
        </TouchableOpacity>
      </View>

      {/* Tabs */}
      <View style={styles.tabs}>
        <TouchableOpacity
          style={[styles.tab, activeTab === 'channels' && styles.tabActive]}
          onPress={() => setActiveTab('channels')}>
          <Text style={[styles.tabText, activeTab === 'channels' && styles.tabTextActive]}>
            Channels
          </Text>
        </TouchableOpacity>
        <TouchableOpacity
          style={[styles.tab, activeTab === 'dms' && styles.tabActive]}
          onPress={() => setActiveTab('dms')}>
          <Text style={[styles.tabText, activeTab === 'dms' && styles.tabTextActive]}>
            Direct Messages
          </Text>
        </TouchableOpacity>
      </View>

      {loading ? (
        <ActivityIndicator style={styles.loader} color="#e94560" size="large" />
      ) : (
        <FlatList
          data={list}
          keyExtractor={(item) => String(item.id)}
          renderItem={renderChannel}
          refreshControl={
            <RefreshControl
              refreshing={refreshing}
              onRefresh={() => {
                setRefreshing(true);
                fetchData();
              }}
              tintColor="#e94560"
            />
          }
          ItemSeparatorComponent={() => <View style={styles.separator} />}
          ListEmptyComponent={
            <Text style={styles.empty}>
              No {activeTab === 'channels' ? 'channels' : 'direct messages'} found
            </Text>
          }
        />
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1, backgroundColor: '#1a1a2e' },
  header: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingTop: 16,
    paddingBottom: 12,
    borderBottomWidth: 1,
    borderBottomColor: '#0f3460',
  },
  headerTitle: { fontSize: 22, fontWeight: '800', color: '#e94560' },
  logoutText: { color: '#888', fontSize: 14 },
  tabs: {
    flexDirection: 'row',
    borderBottomWidth: 1,
    borderBottomColor: '#0f3460',
  },
  tab: {
    flex: 1,
    paddingVertical: 12,
    alignItems: 'center',
  },
  tabActive: {
    borderBottomWidth: 2,
    borderBottomColor: '#e94560',
  },
  tabText: { color: '#888', fontSize: 14, fontWeight: '600' },
  tabTextActive: { color: '#fff' },
  row: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 14,
  },
  rowLeft: { flexDirection: 'row', alignItems: 'center', flex: 1 },
  hash: { fontSize: 18, color: '#888', marginRight: 10, width: 20 },
  channelName: { fontSize: 16, color: '#fff', fontWeight: '600' },
  description: { fontSize: 13, color: '#888', marginTop: 2, maxWidth: 260 },
  separator: { height: 1, backgroundColor: '#0f3460', marginLeft: 46 },
  loader: { marginTop: 60 },
  empty: { textAlign: 'center', color: '#888', marginTop: 60, fontSize: 15 },
});
