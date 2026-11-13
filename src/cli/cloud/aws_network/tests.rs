use super::*;
use crate::cli::cloud::transport;
use std::{cell::RefCell, rc::Rc};

fn inventory() -> Inventory {
    Inventory {
        subnets: vec![
            json!({"SubnetId":"subnet-test","VpcId":"vpc-test","State":"available","AvailableIpAddressCount":10,"CidrBlock":"10.0.0.0/24","AvailabilityZone":"us-west-2a"}),
        ],
        groups: vec![
            json!({"GroupId":"sg-test","VpcId":"vpc-test","IpPermissions":[],"IpPermissionsEgress":[{"IpProtocol":"-1","IpRanges":[{"CidrIp":"0.0.0.0/0"}]}]}),
        ],
        tables: vec![
            json!({"RouteTableId":"rtb-public","VpcId":"vpc-test","Associations":[{"Main":true}],"Routes":[{"DestinationCidrBlock":"0.0.0.0/0","GatewayId":"igw-test","State":"active"}]}),
        ],
        gateways: BTreeSet::from(["igw-test".into()]),
    }
}
/// Shared fake network for existing instance-creation contracts.
pub(in crate::cli::cloud) fn fixture(command: &std::process::Command) -> Option<Vec<u8>> {
    let args: Vec<_> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let i = inventory();
    let value = if args.iter().any(|a| a == "describe-subnets") {
        json!({"Subnets":i.subnets})
    } else if args.iter().any(|a| a == "describe-security-groups") {
        json!({"SecurityGroups":i.groups})
    } else if args.iter().any(|a| a == "describe-route-tables") {
        json!({"RouteTables":i.tables})
    } else if args.iter().any(|a| a == "describe-internet-gateways") {
        json!({"InternetGateways":[{"InternetGatewayId":"igw-test","Attachments":[{"VpcId":"vpc-test","State":"available"}]}]})
    } else {
        return None;
    };
    Some(serde_json::to_vec(&value).unwrap())
}
#[test]
fn explicit_private_route_wins_over_public_main_and_detached_gateway_is_rejected() {
    let mut i = inventory();
    assert!(i.eligible(&i.subnets[0]));
    i.tables.push(json!({"RouteTableId":"rtb-private","Associations":[{"SubnetId":"subnet-test"}],"Routes":[]}));
    assert!(!i.eligible(&i.subnets[0]));
    i.tables.pop();
    i.gateways.clear();
    assert!(!i.eligible(&i.subnets[0]));
}
#[test]
fn wrong_vpc_no_egress_and_inbound_rules_are_rejected() {
    let i = inventory();
    let mut g = i.groups[0].clone();
    assert!(usable_group(&g, "vpc-test"));
    assert!(!usable_group(&g, "other"));
    g["IpPermissionsEgress"] = json!([]);
    assert!(!usable_group(&g, "vpc-test"));
    g = i.groups[0].clone();
    g["IpPermissions"] = json!([{}]);
    assert!(!usable_group(&g, "vpc-test"));
}
#[test]
fn allocation_uses_secondary_cidrs_and_all_occupied_ranges() {
    let v = json!({"CidrBlock":"10.0.0.0/20","CidrBlockAssociationSet":[{"CidrBlock":"10.2.0.0/16","CidrBlockState":{"State":"associated"}}]});
    assert_eq!(
        free_cidr(
            &v,
            &[
                json!({"CidrBlock":"10.0.0.0/20"}),
                json!({"CidrBlock":"10.2.0.0/24"})
            ]
        )
        .unwrap(),
        "10.2.1.0/24"
    );
    assert_eq!(
        free_cidr(&json!({"CidrBlock":"10.3.0.0/25"}), &[]).unwrap(),
        "10.3.0.0/25"
    );
    assert!(free_cidr(
        &json!({"CidrBlock":"10.3.0.0/24"}),
        &[json!({"CidrBlock":"10.3.0.0/24"})]
    )
    .is_err());
}
#[test]
fn blackhole_nat_and_ipv6_defaults_are_not_public_ipv4_routes() {
    let i = inventory();
    for route in [
        json!({"DestinationCidrBlock":"0.0.0.0/0","GatewayId":"igw-test","State":"blackhole"}),
        json!({"DestinationCidrBlock":"0.0.0.0/0","NatGatewayId":"nat-test","State":"active"}),
        json!({"DestinationIpv6CidrBlock":"::/0","GatewayId":"igw-test","State":"active"}),
    ] {
        assert!(!i.public(&json!({"Routes":[route]})));
    }
}
#[test]
fn lost_subnet_reply_and_association_reply_reconcile_without_duplicates() {
    let _context = crate::cli::cloud::tests::Context::new();
    let mut a = crate::cli::cloud::tests::job().account;
    a.subnet = "subnet-test".into();
    let intent = Intent {
        id: "12345678-1234-4234-8234-123456789abc".into(),
        account: a,
        vpc: "vpc-test".into(),
        kind: "subnet".into(),
        request: json!({"CidrBlock":"10.0.1.0/24","AvailabilityZone":"us-west-2a"}),
        route: Some("rtb-public".into()),
        resource: None,
        created_at: now().unwrap(),
        done: false,
    };
    let shared = Rc::new(RefCell::new((0, 0)));
    let state = shared.clone();
    let id = intent.id.clone();
    transport::FAKE.with(|f|*f.borrow_mut()=Some(Box::new(move |c,_| {
        let args:Vec<_>=c.get_args().map(|a|a.to_string_lossy().into_owned()).collect();
        if args.iter().any(|a|a=="get-caller-identity") {return Ok(serde_json::to_vec(&json!({"Account":"123"})).unwrap());}
        if args.iter().any(|a|a=="create-subnet") {state.borrow_mut().0+=1;return Err(error("lost subnet reply"));}
        if args.iter().any(|a|a=="associate-route-table") {state.borrow_mut().1+=1;return Err(error("lost association reply"));}
        if args.iter().any(|a|a=="describe-subnets") {return Ok(serde_json::to_vec(&json!({"Subnets":if state.borrow().0==0 {vec![]}else{vec![json!({"SubnetId":"subnet-created","VpcId":"vpc-test","State":"available","AvailableIpAddressCount":251,"CidrBlock":"10.0.1.0/24","AvailabilityZone":"us-west-2a","Tags":[{"Key":"herdr-network","Value":id}]})]}})).unwrap());}
        if args.iter().any(|a|a=="describe-route-tables") {let mut i=inventory();if state.borrow().1>0 {i.tables[0]["Associations"]=json!([{"Main":true},{"SubnetId":"subnet-created"}]);}return Ok(serde_json::to_vec(&json!({"RouteTables":i.tables})).unwrap());}
        Ok(fixture(c).expect("unexpected network operation"))
    })));
    assert!(start(intent).is_err());
    let recover =
        || complete(read::<Intent>(&root().unwrap().join("network-pending.json")).unwrap());
    assert!(recover().is_err());
    recover().unwrap();
    assert_eq!(*shared.borrow(), (1, 1));
    assert!(
        read::<Intent>(&root().unwrap().join("network-pending.json"))
            .unwrap()
            .done
    );
}

#[test]
fn lost_group_reply_recovers_the_same_owned_group() {
    let _context = crate::cli::cloud::tests::Context::new();
    let id = "12345678-1234-4234-8234-123456789abc";
    let intent = Intent {
        id: id.into(),
        account: crate::cli::cloud::tests::job().account,
        vpc: "vpc-test".into(),
        kind: "security-group".into(),
        request: json!({"GroupName":"herdr-unique"}),
        route: None,
        resource: None,
        created_at: now().unwrap(),
        done: false,
    };
    let count = Rc::new(RefCell::new(0));
    let calls = count.clone();
    transport::FAKE.with(|f| {
        *f.borrow_mut() = Some(Box::new(move |c, _| {
            let args: Vec<_> = c
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            if args.iter().any(|a| a == "get-caller-identity") {
                return Ok(serde_json::to_vec(&json!({"Account":"123"})).unwrap());
            }
            if args.iter().any(|a| a == "create-security-group") {
                *calls.borrow_mut() += 1;
                return Err(error("lost reply"));
            }
            if args.iter().any(|a| a == "describe-security-groups") {
                let mut groups = vec![];
                if *calls.borrow() > 0 {
                    let mut group = inventory().groups.remove(0);
                    group["GroupName"] = json!("HERDR-UNIQUE");
                    group["Tags"] = json!([{"Key":"herdr-network","Value":id}]);
                    groups.push(group);
                }
                return Ok(serde_json::to_vec(&json!({"SecurityGroups":groups})).unwrap());
            }
            Ok(fixture(c).expect("unexpected mutation"))
        }))
    });
    assert!(start(intent).is_err());
    let intent: Intent = read(&root().unwrap().join("network-pending.json")).unwrap();
    assert_eq!(complete(intent).unwrap(), "sg-test");
    assert_eq!(*count.borrow(), 1);
}
#[test]
fn foreign_cidr_collision_is_never_adopted_or_changed() {
    let _context = crate::cli::cloud::tests::Context::new();
    let intent = Intent {
        id: "12345678-1234-4234-8234-123456789abc".into(),
        account: crate::cli::cloud::tests::job().account,
        vpc: "vpc-test".into(),
        kind: "subnet".into(),
        request: json!({"CidrBlock":"10.0.0.0/24","AvailabilityZone":"us-west-2a"}),
        route: Some("rtb-public".into()),
        resource: None,
        created_at: now().unwrap(),
        done: false,
    };
    transport::FAKE.with(|f| {
        *f.borrow_mut() = Some(Box::new(|c, _| {
            let args: Vec<_> = c
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            if args.iter().any(|a| a == "get-caller-identity") {
                return Ok(serde_json::to_vec(&json!({"Account":"123"})).unwrap());
            }
            Ok(fixture(c).expect("must not create or associate a foreign subnet"))
        }))
    });
    assert!(start(intent)
        .unwrap_err()
        .to_string()
        .contains("different resource"));
}
