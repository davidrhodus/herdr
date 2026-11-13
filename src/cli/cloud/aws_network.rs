//! AWS network selection and owned creation, outside the runtime/catalog protocol.
use super::{confirm, input, model::*, safe, transport::*};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    io,
    net::Ipv4Addr,
    time::{Duration, Instant},
};

fn rows(v: &Value, key: &str) -> io::Result<Vec<Value>> {
    v[key]
        .as_array()
        .cloned()
        .ok_or_else(|| error(format!("AWS response is missing {key}")))
}
fn name(v: &Value, key: &str) -> String {
    let label = v["Tags"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|t| t["Key"] == "Name")
        .and_then(|t| t["Value"].as_str())
        .or_else(|| v["GroupName"].as_str())
        .unwrap_or("Unnamed");
    format!(
        "{} · {}",
        safe(label),
        safe(v[key].as_str().unwrap_or("unknown"))
    )
}
fn choose(label: &str, options: &[String], create: bool) -> io::Result<Option<usize>> {
    println!("\n{label}");
    for (i, text) in options.iter().enumerate() {
        println!("  {}  {text}", i + 1);
    }
    if create {
        println!("  N  Create new");
    }
    println!("  0  Cancel");
    loop {
        let answer = input("Choose", if options.is_empty() { "n" } else { "1" })?;
        if answer == "0" {
            return Err(error(
                "Network selection cancelled; existing resources are unchanged",
            ));
        }
        if create && answer.eq_ignore_ascii_case("n") {
            return Ok(None);
        }
        if let Ok(n) = answer.parse::<usize>() {
            if n > 0 && n <= options.len() {
                return Ok(Some(n - 1));
            }
        }
        println!(
            "Choose a listed number{}.",
            if create { " or N" } else { "" }
        );
    }
}
struct Inventory {
    subnets: Vec<Value>,
    groups: Vec<Value>,
    tables: Vec<Value>,
    gateways: BTreeSet<String>,
}
impl Inventory {
    fn load(a: &Account, vpc: &str) -> io::Result<Self> {
        let filter = format!("Name=vpc-id,Values={vpc}");
        let subnets = rows(
            &aws(a, &["ec2", "describe-subnets", "--filters", &filter])?,
            "Subnets",
        )?;
        let groups = rows(
            &aws(
                a,
                &["ec2", "describe-security-groups", "--filters", &filter],
            )?,
            "SecurityGroups",
        )?;
        let tables = rows(
            &aws(a, &["ec2", "describe-route-tables", "--filters", &filter])?,
            "RouteTables",
        )?;
        let gateway_filter = format!("Name=attachment.vpc-id,Values={vpc}");
        let gateways = rows(
            &aws(
                a,
                &[
                    "ec2",
                    "describe-internet-gateways",
                    "--filters",
                    &gateway_filter,
                ],
            )?,
            "InternetGateways",
        )?
        .into_iter()
        .filter(|g| {
            g["Attachments"].as_array().is_some_and(|v| {
                v.iter()
                    .any(|v| v["VpcId"] == vpc && v["State"] == "available")
            })
        })
        .filter_map(|g| g["InternetGatewayId"].as_str().map(str::to_owned))
        .collect();
        Ok(Self {
            subnets,
            groups,
            tables,
            gateways,
        })
    }
    fn public(&self, table: &Value) -> bool {
        table["Routes"].as_array().is_some_and(|routes| {
            routes.iter().any(|r| {
                r["DestinationCidrBlock"] == "0.0.0.0/0"
                    && r["State"] == "active"
                    && r["GatewayId"]
                        .as_str()
                        .is_some_and(|g| self.gateways.contains(g))
            })
        })
    }
    fn effective(&self, subnet: &str) -> Option<&Value> {
        self.tables
            .iter()
            .find(|t| {
                t["Associations"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|a| a["SubnetId"] == subnet))
            })
            .or_else(|| {
                self.tables.iter().find(|t| {
                    t["Associations"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|a| a["Main"] == true))
                })
            })
    }
    fn eligible(&self, s: &Value) -> bool {
        s["State"] == "available"
            && s["AvailableIpAddressCount"].as_u64().is_some_and(|n| n > 0)
            && s["SubnetId"]
                .as_str()
                .and_then(|id| self.effective(id))
                .is_some_and(|t| {
                    self.public(t)
                        && t["Associations"].as_array().is_some_and(|a| {
                            a.iter()
                                .filter(|a| a["SubnetId"] == s["SubnetId"] || a["Main"] == true)
                                .all(|a| {
                                    a["AssociationState"]["State"]
                                        .as_str()
                                        .is_none_or(|state| state == "associated")
                                })
                        })
                })
    }
}
pub(super) fn usable_group(group: &Value, vpc: &str) -> bool {
    group["VpcId"] == vpc
        && group["IpPermissions"].as_array().is_some_and(Vec::is_empty)
        && group["IpPermissionsEgress"]
            .as_array()
            .is_some_and(|rules| {
                rules.iter().any(|r| {
                    r["IpProtocol"] == "-1"
                        && r["IpRanges"]
                            .as_array()
                            .is_some_and(|ranges| ranges.iter().any(|r| r["CidrIp"] == "0.0.0.0/0"))
                })
            })
}
pub(super) fn validate(a: &Account) -> io::Result<()> {
    let subnets = rows(
        &aws(a, &["ec2", "describe-subnets", "--subnet-ids", &a.subnet])?,
        "Subnets",
    )?;
    let subnet = subnets
        .first()
        .filter(|s| s["SubnetId"] == a.subnet)
        .ok_or_else(|| error("Selected subnet no longer exists"))?;
    let vpc = field(subnet, "VpcId")?;
    let inventory = Inventory::load(a, vpc)?;
    if !inventory
        .subnets
        .iter()
        .any(|s| s["SubnetId"] == a.subnet && inventory.eligible(s))
    {
        return Err(error("Subnet no longer has free addresses and an active attached internet gateway. Choose networking again."));
    }
    if !inventory
        .groups
        .iter()
        .any(|g| g["GroupId"] == a.security_group && usable_group(g, vpc))
    {
        return Err(error("Security group must belong to the subnet's VPC, have no inbound rules and allow outbound IPv4 traffic. Choose networking again."));
    }
    Ok(())
}
pub(super) fn select(a: &mut Account) -> io::Result<()> {
    verify_account(a)?;
    let vpcs: Vec<_> = rows(&aws(a, &["ec2", "describe-vpcs"])?, "Vpcs")?
        .into_iter()
        .filter(|v| v["State"] == "available")
        .collect();
    if vpcs.is_empty() {
        return Err(error("No available VPC in this region. Create a VPC with internet access in AWS, or choose another region."));
    }
    let labels = vpcs
        .iter()
        .map(|v| {
            format!(
                "{} · {}{}",
                name(v, "VpcId"),
                safe(v["CidrBlock"].as_str().unwrap_or("")),
                if v["IsDefault"] == true {
                    " · default"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>();
    let vpc =
        &vpcs[choose("Choose a VPC", &labels, false)?.ok_or_else(|| error("Choose a VPC"))?];
    let vpc_id = field(vpc, "VpcId")?;
    let inventory = Inventory::load(a, vpc_id)?;
    let subnets: Vec<_> = inventory
        .subnets
        .iter()
        .filter(|s| inventory.eligible(s))
        .collect();
    let labels = subnets
        .iter()
        .map(|s| {
            format!(
                "{} · {} · {} · {} free IPs",
                name(s, "SubnetId"),
                safe(s["CidrBlock"].as_str().unwrap_or("")),
                safe(s["AvailabilityZone"].as_str().unwrap_or("")),
                s["AvailableIpAddressCount"]
            )
        })
        .collect::<Vec<_>>();
    a.subnet = match choose("Choose a public subnet", &labels, true)? {
        Some(i) => field(subnets[i], "SubnetId")?.into(),
        None => new_subnet(a, vpc, &inventory)?,
    };
    let inventory = Inventory::load(a, vpc_id)?;
    let groups: Vec<_> = inventory
        .groups
        .iter()
        .filter(|g| usable_group(g, vpc_id))
        .collect();
    let labels = groups
        .iter()
        .map(|g| format!("{} · no inbound · outbound allowed", name(g, "GroupId")))
        .collect::<Vec<_>>();
    a.security_group = match choose("Choose a security group", &labels, true)? {
        Some(i) => field(groups[i], "GroupId")?.into(),
        None => new_group(a, vpc_id)?,
    };
    validate(a)?;
    println!("Network selected. Shared routes and existing security-group rules were not changed.");
    Ok(())
}
pub(super) fn configure() -> io::Result<()> {
    let mut a = super::choose_account(&Provider::Aws)?;
    select(&mut a)?;
    let mut s = settings()?;
    let old = s
        .accounts
        .iter_mut()
        .find(|old| {
            old.provider == Provider::Aws
                && old.configuration == a.configuration
                && old.principal == a.principal
                && old.location == a.location
        })
        .ok_or_else(|| error("Account settings changed; configure again"))?;
    *old = a;
    save(&root()?.join("accounts.json"), &s)
}
fn range(cidr: &str) -> io::Result<(u64, u64, u32)> {
    let (ip, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| error("Invalid AWS IPv4 CIDR"))?;
    let ip = u32::from(
        ip.parse::<Ipv4Addr>()
            .map_err(|_| error("Invalid AWS IPv4 address"))?,
    );
    let prefix = prefix
        .parse::<u32>()
        .map_err(|_| error("Invalid AWS CIDR prefix"))?;
    if prefix > 32 {
        return Err(error("Invalid AWS CIDR prefix"));
    }
    let size = 1u64 << (32 - prefix);
    let start = u64::from(ip) / size * size;
    Ok((start, start + size - 1, prefix))
}
fn free_cidr(vpc: &Value, subnets: &[Value]) -> io::Result<String> {
    let mut blocks = vec![field(vpc, "CidrBlock")?.to_owned()];
    for block in vpc["CidrBlockAssociationSet"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v["CidrBlockState"]["State"] == "associated")
    {
        blocks.push(field(block, "CidrBlock")?.into());
    }
    let occupied = subnets
        .iter()
        .map(|s| range(field(s, "CidrBlock")?))
        .collect::<io::Result<Vec<_>>>()?;
    for prefix in 24..=28 {
        let size = 1u64 << (32 - prefix);
        for block in &blocks {
            let (start, end, vpc_prefix) = range(block)?;
            if vpc_prefix > prefix || vpc_prefix < 16 {
                continue;
            }
            let mut next = start;
            while next + size - 1 <= end {
                let last = next + size - 1;
                if !occupied.iter().any(|(s, e, _)| next <= *e && *s <= last) {
                    return Ok(format!("{}/{prefix}", Ipv4Addr::from(next as u32)));
                }
                next += size;
            }
        }
    }
    Err(error("No free IPv4 subnet range remains in this VPC"))
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    id: String,
    account: Account,
    vpc: String,
    kind: String,
    request: Value,
    route: Option<String>,
    resource: Option<String>,
    created_at: u64,
    done: bool,
}
impl Intent {
    fn save(&self) -> io::Result<()> {
        save(&root()?.join("network-pending.json"), self)
    }
}
fn tags(kind: &str, id: &str) -> Value {
    json!([{"ResourceType":kind,"Tags":[{"Key":"Name","Value":format!("herdr-{}",&id[..8])},{"Key":"herdr-network","Value":id}]}])
}
fn start(intent: Intent) -> io::Result<String> {
    let path = root()?.join("network-pending.json");
    if path.exists() && !read::<Intent>(&path)?.done {
        return Err(error(
            "A network creation is pending. Use Resume network creation before creating another.",
        ));
    }
    intent.save()?;
    complete(intent)
}
fn new_subnet(a: &Account, vpc: &Value, inventory: &Inventory) -> io::Result<String> {
    let tables: Vec<_> = inventory
        .tables
        .iter()
        .filter(|t| inventory.public(t))
        .collect();
    if tables.is_empty() {
        return Err(error("This VPC has no public route table with an attached internet gateway. Choose another VPC; Herdr will not alter shared routes."));
    }
    let route = choose(
        "Public route table for the new subnet",
        &tables
            .iter()
            .map(|v| name(v, "RouteTableId"))
            .collect::<Vec<_>>(),
        false,
    )?
    .ok_or_else(|| error("Choose a route table"))?;
    let zones: Vec<_> = rows(
        &aws(a, &["ec2", "describe-availability-zones"])?,
        "AvailabilityZones",
    )?
    .into_iter()
    .filter(|z| {
        z["State"] == "available"
            && z["ZoneType"] == "availability-zone"
            && z["OptInStatus"] != "not-opted-in"
    })
    .collect();
    if zones.is_empty() {
        return Err(error("No available zones in this region"));
    }
    let zone = choose(
        "Availability zone",
        &zones
            .iter()
            .map(|z| safe(z["ZoneName"].as_str().unwrap_or("")))
            .collect::<Vec<_>>(),
        false,
    )?
    .ok_or_else(|| error("Choose a zone"))?;
    let cidr = free_cidr(vpc, &inventory.subnets)?;
    let id = uuid()?;
    let vpc_id = field(vpc, "VpcId")?;
    let zone = field(&zones[zone], "ZoneName")?;
    let route = field(tables[route], "RouteTableId")?;
    if !confirm(&format!("Create subnet {cidr} in {zone}, VPC {vpc_id}, using {route}? It will remain available for reuse when an instance is deleted."),"CREATE")? {return Err(error("Subnet creation cancelled"));}
    start(Intent {
        id: id.clone(),
        account: a.clone(),
        vpc: vpc_id.into(),
        kind: "subnet".into(),
        request: json!({"VpcId":vpc_id,"CidrBlock":cidr,"AvailabilityZone":zone,"TagSpecifications":tags("subnet",&id)}),
        route: Some(route.into()),
        resource: None,
        created_at: now()?,
        done: false,
    })
}
fn new_group(a: &Account, vpc: &str) -> io::Result<String> {
    let id = uuid()?;
    let group = format!("herdr-{}", id);
    if !confirm("Create a Tailscale-only security group in this VPC? No inbound rules; outbound traffic allowed. The group remains reusable when an instance is deleted.","CREATE")? {return Err(error("Security-group creation cancelled"));}
    start(Intent {
        id: id.clone(),
        account: a.clone(),
        vpc: vpc.into(),
        kind: "security-group".into(),
        request: json!({"VpcId":vpc,"GroupName":group,"Description":"Herdr Tailscale servers: no inbound, outbound allowed","TagSpecifications":tags("security-group",&id)}),
        route: None,
        resource: None,
        created_at: now()?,
        done: false,
    })
}
pub(super) fn resume() -> io::Result<()> {
    let path = root()?.join("network-pending.json");
    if !path.exists() {
        println!("No pending network creation.");
        return Ok(());
    }
    let intent: Intent = read(&path)?;
    if intent.done {
        println!("Network creation already completed. Choose Configure AWS networking to use it.");
        return Ok(());
    }
    println!(
        "Pending {} creation in account {} / {}.",
        safe(&intent.kind),
        safe(&intent.account.principal),
        safe(&intent.account.location)
    );
    // Input is kept in the interactive entry point; tests exercise complete()
    // directly so recovery contracts do not depend on a real terminal.
    let action = input("Action: resume / abandon / back", "resume")?;
    if action == "back" {
        return Ok(());
    }
    if action != "resume" && action != "abandon" {
        return Err(error("Choose resume, abandon, or back"));
    }
    if action == "abandon" {
        if confirm("Abandon this pending setup? Any created AWS resources stay in AWS; this does not delete networking or undo routing.","ABANDON")? {
            save(&root()?.join(format!("network-abandoned-{}.json",uuid()?)),&intent)?;
            std::fs::remove_file(&path)?;
            crate::platform::sync_parent_directory(&root()?)?;
        }
        return Ok(());
    }
    let id = complete(intent)?;
    println!("Recovered {id}. Choose Configure AWS networking to select it.");
    Ok(())
}
fn complete(mut intent: Intent) -> io::Result<String> {
    complete_wait(&mut intent, Instant::now())
}
fn complete_wait(intent: &mut Intent, started: Instant) -> io::Result<String> {
    verify_account(&intent.account)?;
    let inventory = Inventory::load(&intent.account, &intent.vpc)?;
    let subnet = intent.kind == "subnet";
    let candidates: Vec<_> = if subnet {
        inventory
            .subnets
            .iter()
            .filter(|s| s["CidrBlock"] == intent.request["CidrBlock"])
            .collect()
    } else {
        inventory
            .groups
            .iter()
            .filter(|g| {
                g["GroupName"].as_str().is_some_and(|n| {
                    intent.request["GroupName"]
                        .as_str()
                        .is_some_and(|expected| n.eq_ignore_ascii_case(expected))
                })
            })
            .collect()
    };
    let key = if subnet { "SubnetId" } else { "GroupId" };
    if let Some(existing) = candidates.first() {
        if candidates.len() != 1
            || existing["VpcId"] != intent.vpc
            || !existing["Tags"].as_array().is_some_and(|t| {
                t.iter()
                    .any(|t| t["Key"] == "herdr-network" && t["Value"] == intent.id)
            })
            || (subnet && existing["AvailabilityZone"] != intent.request["AvailabilityZone"])
        {
            return Err(error("Network name or CIDR is already occupied by a different resource. It was not adopted or changed."));
        }
        let id = field(existing, key)?;
        if intent.resource.as_deref().is_some_and(|known| known != id) {
            return Err(error(
                "Created network resource was replaced; refusing to change it",
            ));
        }
        intent.resource = Some(id.into());
        intent.save()?;
    } else if intent.resource.is_some() {
        if started.elapsed() > Duration::from_secs(60) {
            return Err(error("Created network resource is not visible. Retry Resume later; no replacement was created."));
        }
        std::thread::sleep(Duration::from_secs(2));
        return complete_wait(intent, started);
    } else {
        if now()?
            .checked_sub(intent.created_at)
            .is_none_or(|age| age > 3600)
        {
            return Err(error("Network creation is too old to replay. Inspect AWS for its herdr-network tag before creating a replacement."));
        }
        // Same VPC+CIDR or VPC+group-name is unique in EC2. Never reallocate on a
        // conflict: a lost reply converges through the ownership-checked path above.
        let path = root()?.join("network-request.json");
        save(&path, &intent.request)?;
        let operation = if subnet {
            "create-subnet"
        } else {
            "create-security-group"
        };
        let reply = aws(
            &intent.account,
            &[
                "ec2",
                operation,
                "--cli-input-json",
                &format!("file://{}", path.display()),
            ],
        )?;
        let value = if subnet { &reply["Subnet"] } else { &reply };
        intent.resource = Some(field(value, key)?.into());
        intent.save()?;
        // Reconcile tags, availability and ownership after the reply as well.
        return complete_wait(intent, started);
    }
    let id = intent
        .resource
        .clone()
        .ok_or_else(|| error("Missing network identity"))?;
    if subnet {
        let route = intent
            .route
            .as_deref()
            .ok_or_else(|| error("Missing subnet route table"))?;
        if !inventory
            .tables
            .iter()
            .any(|t| t["RouteTableId"] == route && inventory.public(t))
        {
            return Err(error(
                "Selected public route or attached gateway changed; no routing was altered.",
            ));
        }
        if let Some(table) = inventory.tables.iter().find(|t| {
            t["Associations"]
                .as_array()
                .is_some_and(|a| a.iter().any(|a| a["SubnetId"] == id))
        }) {
            if table["RouteTableId"] != route {
                return Err(error("Subnet was associated with another route table. Existing association left unchanged."));
            }
        } else {
            aws(
                &intent.account,
                &[
                    "ec2",
                    "associate-route-table",
                    "--subnet-id",
                    &id,
                    "--route-table-id",
                    route,
                ],
            )?;
        }
        let start = Instant::now();
        loop {
            let live = Inventory::load(&intent.account, &intent.vpc)?;
            if live
                .subnets
                .iter()
                .any(|s| s["SubnetId"] == id && live.eligible(s))
                && live
                    .effective(&id)
                    .is_some_and(|t| t["RouteTableId"] == route)
            {
                break;
            }
            if start.elapsed() > Duration::from_secs(60) {
                return Err(error(
                    "Subnet routing is not ready yet. Resume network creation later.",
                ));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    } else if !inventory
        .groups
        .iter()
        .any(|g| g["GroupId"] == id && usable_group(g, &intent.vpc))
    {
        return Err(error(
            "Created security group rules changed; it was not used or altered.",
        ));
    }
    intent.done = true;
    intent.save()?;
    println!("Network resource ready: {id}");
    Ok(id)
}

#[cfg(test)]
pub(super) mod tests;
