use super::sample::Sample;
use log::{debug, info, trace};
use rand::SeedableRng;
use rand::{CryptoRng, Rng, RngCore};
use rand_chacha::ChaCha20Rng;
use rand_distr::num_traits::ToPrimitive;
use rand_distr::{Bernoulli, Distribution};
use raw_ipa::helpers::models::{
    Event as EEvent, SecretSharable, SecretShare, SourceEvent as ESourceEvent,
    TriggerEvent as ETriggerEvent,
};
use serde::{Deserialize, Serialize};
use std::io;
use std::time::Duration;

const DAYS_IN_EPOCH: u64 = 7;

#[derive(Clone)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct Event {
    // For this tool, we'll fix the length of a matchkey to u64
    pub matchkeys: Vec<u64>,
    pub epoch: u8,
    pub timestamp: u32,
}

#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct SourceEvent {
    pub event: Event,
    pub breakdown_key: String,
}

#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct TriggerEvent {
    pub event: Event,
    pub value: u32,
    pub zkp: String,
}

#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub enum EventType {
    // Source event in clear
    S(SourceEvent),
    // Trigger event in clear
    T(TriggerEvent),
    // Source event in cipher if --secret-share option is enabled
    ES(ESourceEvent),
    // Trigger event in cipher if --secret-share option is enabled
    ET(ETriggerEvent),
}

struct GenEventParams {
    devices: u8,
    impressions: u8,
    conversions: u8,
    epoch: u8,
    breakdown_key: String,
}

// TODO: Currently, users are mutually exclusive in each ad loop (i.e. User A in ad X will never appear in other ads).
// We need to generate events from same users across ads (but how often should a user appear in different ads?)
// "Ads" doesn't mean FB's L3 ads. It could be ads from different businesses.

pub fn generate_events<W: io::Write>(
    total_count: u32,
    epoch: u8,
    secret_share: bool,
    seed: &Option<u64>,
    out: &mut W,
) -> (u32, u32) {
    let mut rng = match seed {
        None => ChaCha20Rng::from_entropy(),
        Some(seed) => ChaCha20Rng::seed_from_u64(*seed),
    };
    debug!("seed: {:?}", rng.get_seed());

    // Separate RNG for generating secret shares
    let mut ss_rng = match seed {
        None => ChaCha20Rng::from_entropy(),
        Some(seed) => ChaCha20Rng::seed_from_u64(*seed),
    };

    let sample = Sample::new();

    let mut ad_count = 0;
    let mut event_count = 0;
    let mut total_impressions = 0;
    let mut total_conversions = 0;

    // Simulate impressions and conversions from an ad.
    // We define "ad" as a group of impressions and conversions from targeted users who are selected by predefined
    // breakdowns such as age, gender and locations.
    loop {
        ad_count += 1;
        debug!("ad: {}", ad_count);

        // TODO: 99.97% queries in ads manager account for L1-3 breakdown only. For now, we'll do 1 ad = 1 breakdown key
        let ad_id: u32 = rng.gen();

        // Number of unique people who saw the ad
        let reach = sample.reach_per_ad(&mut rng);
        debug!("reach: {}", reach);

        // CVR for the ad
        let cvr = sample.cvr_per_ad_account(&mut rng);
        debug!("CVR: {}", cvr);

        for _ in 0..reach {
            // # of devices == # of matchkeys
            let devices = sample.devices_per_user(&mut rng);
            trace!("devices per user: {}", devices);

            let impressions = sample.impression_per_user(&mut rng);
            trace!("impressions per user: {}", impressions);

            // Probabilistically decide whether this user has converted or not
            let conversions = if Bernoulli::new(cvr).unwrap().sample(&mut rng) {
                sample.conversion_per_user(&mut rng)
            } else {
                0
            };
            trace!("conversions per user: {}", conversions);

            let events = gen_events(
                &GenEventParams {
                    devices,
                    impressions,
                    conversions,
                    epoch,
                    breakdown_key: ad_id.to_string(),
                },
                secret_share,
                &sample,
                &mut rng,
                &mut ss_rng,
            );

            total_impressions += impressions.to_u32().unwrap();
            total_conversions += conversions.to_u32().unwrap();

            for e in events {
                out.write_all(serde_json::to_string(&e).unwrap().as_bytes())
                    .unwrap();
                writeln!(out).unwrap();

                event_count += 1;
                if event_count % 10000 == 0 {
                    info!("{}", event_count);
                }
                if event_count >= total_count {
                    return (total_impressions, total_conversions);
                }
            }
        }
    }
}

fn gen_events<R: RngCore + CryptoRng>(
    params: &GenEventParams,
    secret_share: bool,
    sample: &Sample,
    rng: &mut R,
    ss_rng: &mut R,
) -> Vec<EventType> {
    let mut events: Vec<EventType> = Vec::new();

    let matchkeys = gen_matchkeys(params.devices, rng);
    let mut ss_mks: Vec<SecretShare> = Vec::new();

    if secret_share {
        for mk in &matchkeys {
            // Currently, all geneerated match keys are set in all source events from the same user. This is an ideal
            // scenario where all devices are used equally. In reality, however, that isn't the case. Should we pick
            // a few match keys out from the events?
            ss_mks.push(mk.split(ss_rng));
        }
    }

    // Randomly choose a datetime of the first impression in [0..DAYS_IN_EPOCH)
    // TODO: Assume that impressions happen any time within the epoch
    let mut last_impression = Duration::new(rng.gen_range(0..DAYS_IN_EPOCH * 24 * 60 * 60), 0);

    for _ in 0..params.impressions {
        let t = last_impression + sample.impressions_time_diff(rng);

        if secret_share {
            events.push(EventType::ES(ESourceEvent {
                event: EEvent {
                    matchkeys: ss_mks.clone(),
                    //TODO: Carry to next epoch if timestamp > DAYS_IN_EPOCH
                    epoch: params.epoch,
                    timestamp: u32::try_from(t.as_secs()).unwrap().split(ss_rng),
                },
                breakdown_key: params.breakdown_key.clone(),
            }));
        } else {
            events.push(EventType::S(SourceEvent {
                event: Event {
                    matchkeys: matchkeys.clone(),
                    //TODO: Carry to next epoch if timestamp > DAYS_IN_EPOCH
                    epoch: params.epoch,
                    timestamp: u32::try_from(t.as_secs()).unwrap(),
                },
                breakdown_key: params.breakdown_key.clone(),
            }));
        }

        last_impression = t;
    }

    // TODO: How should we simulate a case where there are multiple conversions and impressions in between? e.g. i -> i -> c -> i -> c

    let mut last_conversion = last_impression;

    for _ in 0..params.conversions {
        let conversion_value = sample.conversion_value_per_ad(rng);
        let t = last_conversion + sample.conversions_time_diff(rng);

        if secret_share {
            events.push(EventType::ET(ETriggerEvent {
                event: EEvent {
                    matchkeys: ss_mks.clone(),
                    //TODO: Carry to next epoch if timestamp > DAYS_IN_EPOCH
                    epoch: params.epoch,
                    timestamp: u32::try_from(t.as_secs()).unwrap().split(ss_rng),
                },
                value: conversion_value.split(ss_rng),
                zkp: String::from("zkp"),
            }));
        } else {
            events.push(EventType::T(TriggerEvent {
                event: Event {
                    matchkeys: matchkeys.clone(),
                    //TODO: Carry to next epoch if timestamp > DAYS_IN_EPOCH
                    epoch: params.epoch,
                    timestamp: u32::try_from(t.as_secs()).unwrap(),
                },
                value: conversion_value,
                zkp: String::from("zkp"),
            }));
        }

        last_conversion = t;
    }

    events
}

fn gen_matchkeys<R: RngCore + CryptoRng>(count: u8, rng: &mut R) -> Vec<u64> {
    let mut mks = Vec::new();

    for _ in 0..count {
        mks.push(rng.gen::<u64>());
    }
    mks
}

#[cfg(test)]
mod tests {
    use super::{generate_events, EventType};
    use rand::Rng;
    use rand_distr::Alphanumeric;
    use raw_ipa::helpers::models::SecretSharable;
    use std::env::temp_dir;
    use std::fs::{self, File};
    use std::io::prelude::*;
    use std::io::{BufReader, Read, Write};
    use std::path::PathBuf;

    fn gen_temp_file_path() -> PathBuf {
        let mut dir = temp_dir();
        let file: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(16)
            .map(char::from)
            .collect();

        dir.push(file);
        dir
    }

    #[test]
    fn same_seed_generates_same_output() {
        let temp1 = gen_temp_file_path();
        let temp2 = gen_temp_file_path();

        let seed = Some(0);
        let mut out1 = Box::new(File::create(&temp1).unwrap()) as Box<dyn Write>;
        let mut out2 = Box::new(File::create(&temp2).unwrap()) as Box<dyn Write>;

        generate_events(100, 0, false, &seed, &mut out1);
        generate_events(100, 0, false, &seed, &mut out2);

        let mut file1 = File::open(&temp1).unwrap();
        let mut file2 = File::open(&temp2).unwrap();
        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        file1.read_to_end(&mut buf1).unwrap();
        file2.read_to_end(&mut buf2).unwrap();

        assert!(buf1.eq(&buf2));

        fs::remove_file(&temp1).unwrap();
        fs::remove_file(&temp2).unwrap();
    }

    #[test]
    fn same_seed_generates_same_ss_output() {
        let temp1 = gen_temp_file_path();
        let temp2 = gen_temp_file_path();

        let seed = Some(0);
        let mut out1 = Box::new(File::create(&temp1).unwrap()) as Box<dyn Write>;
        let mut out2 = Box::new(File::create(&temp2).unwrap()) as Box<dyn Write>;

        generate_events(100, 0, false, &seed, &mut out1);
        generate_events(100, 0, false, &seed, &mut out2);

        let mut file1 = File::open(&temp1).unwrap();
        let mut file2 = File::open(&temp2).unwrap();
        let mut buf1 = Vec::new();
        let mut buf2 = Vec::new();

        file1.read_to_end(&mut buf1).unwrap();
        file2.read_to_end(&mut buf2).unwrap();

        assert!(buf1.eq(&buf2));

        fs::remove_file(&temp1).unwrap();
        fs::remove_file(&temp2).unwrap();
    }

    #[test]
    fn same_seed_ss_matchkeys_and_plain_matchkeys_are_same() {
        let temp1 = gen_temp_file_path();
        let temp2 = gen_temp_file_path();

        let seed = Some(0);
        let mut out1 = Box::new(File::create(&temp1).unwrap()) as Box<dyn Write>;
        let mut out2 = Box::new(File::create(&temp2).unwrap()) as Box<dyn Write>;

        generate_events(10000, 0, false, &seed, &mut out1);
        generate_events(10000, 0, true, &seed, &mut out2);

        let file1 = File::open(&temp1).unwrap();
        let file2 = File::open(&temp2).unwrap();
        let buf1 = BufReader::new(file1);
        let mut buf2 = BufReader::new(file2);

        for line in buf1.lines() {
            let l1 = line.unwrap();
            let mut l2 = String::new();
            buf2.read_line(&mut l2).unwrap();

            let e1 = serde_json::from_str::<EventType>(&l1).unwrap();
            let e2 = serde_json::from_str::<EventType>(&l2).unwrap();

            match e1 {
                EventType::S(s) => {
                    if let EventType::ES(es) = e2 {
                        for (k, v) in s.event.matchkeys.iter().enumerate() {
                            let ssm = u64::combine(&es.event.matchkeys[k]).unwrap();
                            assert!(*v == ssm);
                        }

                        let timestamp = u32::combine(&es.event.timestamp).unwrap();
                        assert!(s.event.timestamp == timestamp);
                        assert!(s.breakdown_key == es.breakdown_key);
                        assert!(s.event.epoch == es.event.epoch);
                    } else {
                        unreachable!();
                    }
                }

                EventType::T(t) => {
                    if let EventType::ET(et) = e2 {
                        for (k, v) in t.event.matchkeys.iter().enumerate() {
                            let matchkey = u64::combine(&et.event.matchkeys[k]).unwrap();
                            assert!(*v == matchkey);
                        }

                        let timestamp = u32::combine(&et.event.timestamp).unwrap();
                        let value = u32::combine(&et.value).unwrap();
                        assert!(t.event.timestamp == timestamp);
                        assert!(t.value == value);
                        assert!(t.zkp == et.zkp);
                        assert!(t.event.epoch == et.event.epoch);
                    } else {
                        unreachable!();
                    }
                }

                EventType::ES(_) | EventType::ET(_) => unreachable!(),
            }
        }

        fs::remove_file(&temp1).unwrap();
        fs::remove_file(&temp2).unwrap();
    }
}
